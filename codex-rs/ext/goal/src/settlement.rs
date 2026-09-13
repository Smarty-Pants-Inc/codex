/// One native turn settlement, never restored from persisted history.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) enum Settlement {
    #[default]
    None,
    Running(String),
    Completed,
}

impl Settlement {
    pub(crate) fn finish(&mut self, turn_id: &str) {
        if matches!(self, Self::Running(id) if id == turn_id) {
            *self = Self::Completed;
        }
    }

    /// Consume before native admission, including rejected or uncertain starts.
    pub(crate) fn take_completed(&mut self) -> bool {
        matches!(std::mem::take(self), Self::Completed)
    }
}
