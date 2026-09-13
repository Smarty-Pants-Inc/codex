use codex_protocol::turn_input::TurnStartGuard;

/// One native turn settlement, never restored from persisted history.
#[derive(Debug, Default)]
pub(crate) enum Settlement {
    #[default]
    None,
    Running {
        turn_id: String,
        guard: TurnStartGuard,
    },
    Completed(TurnStartGuard),
    Claimed(TurnStartGuard),
}

impl Settlement {
    pub(crate) fn start(&mut self, turn_id: &str) {
        self.stop();
        *self = Self::Running {
            turn_id: turn_id.to_string(),
            guard: TurnStartGuard::default(),
        };
    }

    pub(crate) fn stop(&mut self) {
        match std::mem::take(self) {
            Self::Running { guard, .. } | Self::Completed(guard) | Self::Claimed(guard) => {
                guard.revoke()
            }
            Self::None => {}
        }
    }

    pub(crate) fn guard_for_turn(&self, turn_id: &str) -> Option<TurnStartGuard> {
        match self {
            Self::Running {
                turn_id: current,
                guard,
            } if current == turn_id => Some(guard.clone()),
            Self::None | Self::Running { .. } | Self::Completed(_) | Self::Claimed(_) => None,
        }
    }

    pub(crate) fn finish(&mut self, turn_id: &str) {
        if let Some(guard) = self.guard_for_turn(turn_id) {
            *self = Self::Completed(guard);
        }
    }

    /// Consume before native admission; retain revocation authority over the queued request.
    pub(crate) fn take_completed(&mut self) -> Option<TurnStartGuard> {
        if let Self::Completed(guard) = self {
            let guard = guard.clone();
            *self = Self::Claimed(guard.clone());
            Some(guard)
        } else {
            None
        }
    }

    pub(crate) fn legacy_resume(&mut self) -> TurnStartGuard {
        match self {
            Self::None => {
                let guard = TurnStartGuard::default();
                *self = Self::Claimed(guard.clone());
                guard
            }
            Self::Running { guard, .. } | Self::Completed(guard) | Self::Claimed(guard) => {
                guard.clone()
            }
        }
    }
}
