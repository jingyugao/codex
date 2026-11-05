//! In-memory, cooperative turn suspension. Never cancels a model request or an admitted tool.
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use tokio::sync::watch;

#[derive(Debug)]
struct State {
    generation: u64,
    requested: bool,
    paused: bool,
    claimed: bool,
    announced: bool,
    resuming: bool,
    closed: bool,
    at_boundary: bool,
    models: usize,
    tools: usize,
}

/// The lifetime of this controller is exactly one live turn. It is not persisted.
#[derive(Debug)]
pub struct TurnControl {
    state: Mutex<State>,
    changed: watch::Sender<u64>,
    publication: Arc<tokio::sync::Mutex<()>>,
}

impl Default for TurnControl {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                generation: 0,
                requested: false,
                paused: false,
                claimed: false,
                announced: false,
                resuming: false,
                closed: false,
                at_boundary: true,
                models: 0,
                tools: 0,
            }),
            changed: watch::channel(0).0,
            publication: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
}

impl TurnControl {
    pub(crate) fn status(&self) -> Result<&'static str, String> {
        let state = self.state();
        if state.closed {
            return Err("turn has ended".into());
        }
        Ok(if state.resuming {
            "resuming"
        } else if state.paused {
            "paused"
        } else if state.requested {
            "pauseRequested"
        } else {
            "running"
        })
    }

    #[expect(
        clippy::expect_used,
        reason = "poisoned turn admission state must not admit work"
    )]
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("turn control poisoned")
    }

    fn publish(&self, state: &mut State) {
        if !state.closed
            && state.requested
            && state.at_boundary
            && state.models == 0
            && state.tools == 0
        {
            state.paused = true;
        }
        self.changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    pub(crate) fn request(self: &Arc<Self>) -> Result<SafeInterruptHandle, String> {
        let mut state = self.state();
        if state.closed {
            return Err("turn has already ended".into());
        }
        if !state.requested {
            state.generation += 1;
            state.requested = true;
            state.claimed = false;
            state.announced = false;
        }
        let generation = state.generation;
        self.publish(&mut state);
        Ok(SafeInterruptHandle {
            control: Arc::clone(self),
            generation,
            publication: Mutex::new(None),
        })
    }

    /// Claim a resume without opening admission. The caller queues its resumed event before release.
    pub(crate) async fn prepare_resume(self: &Arc<Self>) -> Result<ResumeHandle, String> {
        let publication = Arc::clone(&self.publication).lock_owned().await;
        let mut state = self.state();
        if state.closed || !state.paused || !state.announced || state.resuming {
            return Err("turn is not paused, or its paused event has not been published".into());
        }
        state.resuming = true;
        Ok(ResumeHandle {
            control: Arc::clone(self),
            generation: state.generation,
            _publication: publication,
        })
    }

    pub(crate) async fn enter_model(self: &Arc<Self>) -> Option<WorkGuard> {
        self.enter(true, true).await
    }

    pub(crate) async fn enter_tool(self: &Arc<Self>) -> Option<WorkGuard> {
        self.enter(false, true).await
    }

    /// Code-mode envelopes can wait on a nested tool parked at this same gate.
    /// Gate new envelopes, but drain their actual host calls rather than counting the envelope.
    pub(crate) async fn enter_tool_container(self: &Arc<Self>) -> Option<WorkGuard> {
        self.enter(false, false).await
    }

    async fn enter(self: &Arc<Self>, model: bool, counted: bool) -> Option<WorkGuard> {
        let mut changed = self.changed.subscribe();
        loop {
            {
                let mut state = self.state();
                if state.closed {
                    return None;
                }
                if !state.requested {
                    if model {
                        state.models += 1;
                        state.at_boundary = false;
                    } else if counted {
                        state.tools += 1;
                    }
                    return Some(WorkGuard {
                        control: Arc::clone(self),
                        model,
                        counted,
                        boundary: true,
                    });
                }
            }
            if changed.changed().await.is_err() {
                return None;
            }
        }
    }

    pub(crate) async fn close(&self) {
        let _publication = self.publication.lock().await;
        let mut state = self.state();
        state.closed = true;
        state.paused = false;
        self.publish(&mut state);
    }
}

/// Returned only for the matching active regular turn. Duplicate requests share a generation.
pub struct SafeInterruptHandle {
    control: Arc<TurnControl>,
    generation: u64,
    publication: Mutex<Option<tokio::sync::OwnedMutexGuard<()>>>,
}

impl SafeInterruptHandle {
    /// Exactly one caller may announce a pause generation. Completion/abort wakes all waiters.
    pub async fn wait_paused(&self) -> bool {
        let mut changed = self.control.changed.subscribe();
        loop {
            let ready = {
                let state = self.control.state();
                if state.closed || state.generation != self.generation || state.claimed {
                    return false;
                }
                state.paused
            };
            if !ready {
                if changed.changed().await.is_err() {
                    return false;
                }
                continue;
            }
            let publication = Arc::clone(&self.control.publication).lock_owned().await;
            let mut state = self.control.state();
            if state.closed || state.generation != self.generation || state.claimed {
                return false;
            }
            state.claimed = true;
            *self
                .publication
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(publication);
            return true;
        }
    }

    /// Call synchronously after queueing turn/paused, before allowing resume.
    pub fn mark_announced(&self) {
        let mut state = self.control.state();
        if !state.closed && state.paused && state.generation == self.generation {
            state.announced = true;
            self.control.publish(&mut state);
        }
        drop(state);
        self.publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

/// Keeps tools parked until the resumed notification has entered the outgoing queue.
pub struct ResumeHandle {
    control: Arc<TurnControl>,
    generation: u64,
    _publication: tokio::sync::OwnedMutexGuard<()>,
}

impl ResumeHandle {
    pub fn release(self) -> Result<(), String> {
        let mut state = self.control.state();
        if state.closed || !state.paused || state.generation != self.generation {
            return Err("paused turn changed before resume".into());
        }
        state.requested = false;
        state.paused = false;
        state.resuming = false;
        self.control.publish(&mut state);
        Ok(())
    }
}

impl Drop for ResumeHandle {
    fn drop(&mut self) {
        let mut state = self.control.state();
        if state.generation == self.generation && state.resuming {
            state.resuming = false;
            self.control.publish(&mut state);
        }
    }
}

/// Ordinary interrupts cancel execution; a subsequent continuation starts a new turn.
pub(crate) struct InterruptedTurn {
    pub(crate) turn_id: String,
    pub(crate) env: codex_protocol::turn_input::ToolEnv,
}

pub enum TurnResumeOutcome {
    Paused(ResumeHandle),
    Restarted { turn_id: String },
}

pub(crate) struct WorkGuard {
    control: Arc<TurnControl>,
    model: bool,
    counted: bool,
    boundary: bool,
}

impl WorkGuard {
    /// A final answer is allowed to complete normally instead of producing a spurious pause.
    pub(crate) fn finish_model(mut self, has_follow_up: bool) {
        self.boundary = has_follow_up;
    }
}

impl Drop for WorkGuard {
    fn drop(&mut self) {
        let mut state = self.control.state();
        if self.model {
            state.models -= 1;
            state.at_boundary = self.boundary;
        } else if self.counted {
            state.tools -= 1;
        }
        self.control.publish(&mut state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn pause_drains_model_and_admitted_tools_and_resumes_pending_call() {
        let control = Arc::new(TurnControl::default());
        let model = control.enter_model().await.unwrap();
        let running = control.enter_tool().await.unwrap();
        let pause = control.request().unwrap();
        let duplicate = control.request().unwrap();
        let pending_control = Arc::clone(&control);
        let mut pending = tokio::spawn(async move { pending_control.enter_tool().await });
        model.finish_model(true);
        assert!(!control.state().paused);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut pending)
                .await
                .is_err()
        );
        drop(running);
        assert!(pause.wait_paused().await);
        assert!(!duplicate.wait_paused().await);
        pause.mark_announced();
        let resume = control.prepare_resume().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut pending)
                .await
                .is_err()
        );
        resume.release().unwrap();
        assert!(pending.await.unwrap().is_some());
    }

    #[tokio::test]
    async fn final_answer_and_hard_interrupt_do_not_leave_a_stuck_pause() {
        let control = Arc::new(TurnControl::default());
        let model = control.enter_model().await.unwrap();
        let pause = control.request().unwrap();
        model.finish_model(false);
        assert!(!control.state().paused);
        control.close().await;
        assert!(!pause.wait_paused().await);
        assert!(control.enter_tool().await.is_none());
    }

    #[tokio::test]
    async fn terminal_event_waits_until_pause_announcement_is_queued() {
        let control = Arc::new(TurnControl::default());
        let pause = control.request().unwrap();
        assert!(pause.wait_paused().await);
        let closing = Arc::clone(&control);
        let close = tokio::spawn(async move { closing.close().await });
        tokio::task::yield_now().await;
        assert!(!close.is_finished());
        pause.mark_announced();
        close.await.unwrap();
        assert!(control.status().is_err());
    }

    #[tokio::test]
    async fn code_mode_envelope_does_not_deadlock_a_parked_nested_tool() {
        let control = Arc::new(TurnControl::default());
        let _envelope = control.enter_tool_container().await.unwrap();
        let pause = control.request().unwrap();
        assert!(pause.wait_paused().await);
        pause.mark_announced();
        let nested_control = Arc::clone(&control);
        let pending = tokio::spawn(async move { nested_control.enter_tool().await });
        tokio::task::yield_now().await;
        assert!(!pending.is_finished());
        control.prepare_resume().await.unwrap().release().unwrap();
        assert!(pending.await.unwrap().is_some());
    }
}
