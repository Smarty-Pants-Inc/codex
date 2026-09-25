//! Per-item ownership of reasoning and commentary in turns that voice and typed input share.
//!
//! Voice and typed input can steer the same turn, so ownership is recorded per item when the
//! item starts: the `realtime` trigger starts a turn as voice, an ordinary user item hands it to
//! typed input and a `<realtime_delegation>` marker hands it back to voice. An item keeps the
//! owner it started with through its updates and completion. Live handling, saved-turn replay
//! and the buffered-event filter all read this one map, so they agree on what stays private.

use super::is_private_realtime_agent_item;
use super::is_realtime_triggered_turn;
use super::realtime_delegation_input;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::Turn;
use std::collections::HashMap;
use std::collections::VecDeque;

// ponytail: insertion order bounds memory; only recent turns receive more events. Revisit
// if a thread can keep more turns than this in progress at once.
const MAX_TRACKED_TURNS: usize = 64;
const MAX_TURN_ID_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    Typed,
    Voice,
}

#[derive(Clone, Debug)]
struct TurnOwners {
    // The owner that the turn's next item starts with.
    owner: Owner,
    items: HashMap<String, Owner>,
}

/// Who owns each item of the recent turns, recorded when the item starts.
#[derive(Clone, Debug, Default)]
pub(crate) struct RealtimeItemOwners {
    turns: VecDeque<(String, TurnOwners)>,
}

fn is_handoff_marker(item: &ThreadItem) -> bool {
    matches!(item, ThreadItem::UserMessage { content, .. }
        if realtime_delegation_input(content).is_some())
}

impl RealtimeItemOwners {
    fn turn(&self, turn_id: &str) -> Option<&TurnOwners> {
        self.turns
            .iter()
            .find_map(|(id, turn)| (id == turn_id).then_some(turn))
    }

    /// Returns the turn's entry, creating it with `owner` when the turn is not tracked yet.
    fn turn_mut(&mut self, turn_id: &str, owner: Owner) -> Option<&mut TurnOwners> {
        if turn_id.len() > MAX_TURN_ID_BYTES {
            return None;
        }
        let index = match self.turns.iter().position(|(id, _)| id == turn_id) {
            Some(index) => index,
            None => {
                if self.turns.len() == MAX_TRACKED_TURNS {
                    self.turns.pop_front();
                }
                self.turns.push_back((
                    turn_id.to_string(),
                    TurnOwners {
                        owner,
                        items: HashMap::new(),
                    },
                ));
                self.turns.len() - 1
            }
        };
        self.turns.get_mut(index).map(|(_, turn)| turn)
    }

    pub(crate) fn tracks_turn(&self, turn_id: &str) -> bool {
        self.turn(turn_id).is_some()
    }

    /// A `realtime` trigger starts its turn as voice. A turn that is already tracked keeps
    /// its current owner: the trigger only sets the owner of the turn's first item.
    pub(crate) fn note_turn_started(&mut self, turn: &Turn) {
        if is_realtime_triggered_turn(turn) {
            self.turn_mut(&turn.id, Owner::Voice);
        }
    }

    /// Voice owns the turn's next items. Use this when a handoff is known but its position
    /// among the items is not, for example after its marker left the bounded event buffer.
    pub(crate) fn note_voice_turn(&mut self, turn_id: &str) {
        if let Some(turn) = self.turn_mut(turn_id, Owner::Voice) {
            turn.owner = Owner::Voice;
        }
    }

    /// Records an item that started before the turn's handoff to voice.
    pub(crate) fn note_typed_item(&mut self, turn_id: &str, item_id: &str) {
        if let Some(turn) = self.turn_mut(turn_id, Owner::Typed) {
            turn.items.insert(item_id.to_string(), Owner::Typed);
        }
    }

    /// Records the owner of `item` the first time it is seen, normally at its start.
    ///
    /// A user item changes the turn's owner for the items that follow it. An item seen again
    /// (an update or completion) keeps the owner it started with.
    pub(crate) fn note_item(&mut self, turn_id: &str, item: &ThreadItem) {
        let Some(turn) = self.turn_mut(turn_id, Owner::Typed) else {
            return;
        };
        if turn.items.contains_key(item.id()) {
            return;
        }
        if matches!(item, ThreadItem::UserMessage { .. }) {
            turn.owner = if is_handoff_marker(item) {
                Owner::Voice
            } else {
                Owner::Typed
            };
        }
        turn.items.insert(item.id().to_string(), turn.owner);
    }

    /// Records the ownership changes of an app-server notification, in event order.
    pub(crate) fn note_notification(&mut self, notification: &ServerNotification) {
        match notification {
            ServerNotification::TurnStarted(n) => self.note_turn_started(&n.turn),
            ServerNotification::ItemStarted(n) => self.note_item(&n.turn_id, &n.item),
            ServerNotification::ItemCompleted(n) => self.note_item(&n.turn_id, &n.item),
            _ => {}
        }
    }

    pub(crate) fn voice_owns_turn(&self, turn_id: &str) -> bool {
        self.turn(turn_id)
            .is_some_and(|turn| turn.owner == Owner::Voice)
    }

    /// Whether voice owns the item. An item not seen yet takes the turn's current owner.
    pub(crate) fn voice_owns_item(&self, turn_id: &str, item_id: &str) -> bool {
        match self.turn(turn_id).and_then(|turn| turn.items.get(item_id)) {
            Some(owner) => *owner == Owner::Voice,
            None => self.voice_owns_turn(turn_id),
        }
    }

    /// Whether `item` is reasoning or commentary that voice owns and must not render.
    pub(crate) fn hides(&self, turn_id: &str, item: &ThreadItem) -> bool {
        is_private_realtime_agent_item(item) && self.voice_owns_item(turn_id, item.id())
    }

    /// Walks a saved turn as if its items started in order and returns which items may
    /// render. Items that were already seen keep their recorded owner. A new turn ends with
    /// the owner of its last handoff, so later events for the turn continue from there; a
    /// tracked turn keeps its current owner, which newer events already set.
    ///
    /// Items without a trigger or marker cannot show where a handoff happened, so a turn that
    /// is already voice-owned hides all of its private items.
    fn saved_item_visibility(&mut self, turn: &Turn) -> Vec<bool> {
        let triggered = is_realtime_triggered_turn(turn);
        if !triggered && !turn.items.iter().any(is_handoff_marker) {
            return turn
                .items
                .iter()
                .map(|item| !self.hides(&turn.id, item))
                .collect();
        }
        let mut owner = if triggered {
            Owner::Voice
        } else {
            Owner::Typed
        };
        let tracked = self.tracks_turn(&turn.id);
        let Some(entry) = self.turn_mut(&turn.id, owner) else {
            return vec![true; turn.items.len()];
        };
        let visibility = turn
            .items
            .iter()
            .map(|item| {
                if matches!(item, ThreadItem::UserMessage { .. }) {
                    owner = if is_handoff_marker(item) {
                        Owner::Voice
                    } else {
                        Owner::Typed
                    };
                }
                let item_owner = *entry.items.entry(item.id().to_string()).or_insert(owner);
                item_owner == Owner::Typed || !is_private_realtime_agent_item(item)
            })
            .collect();
        if !tracked {
            entry.owner = owner;
        }
        visibility
    }

    /// Drops a saved turn's voice-private items; see [`Self::saved_item_visibility`].
    pub(crate) fn retain_visible_items(&mut self, turn: &mut Turn) {
        let mut visibility = self.saved_item_visibility(turn).into_iter();
        turn.items
            .retain(|_| visibility.next().unwrap_or(/*default*/ true));
    }

    /// Returns each saved turn's item visibility; see [`Self::saved_item_visibility`].
    ///
    /// Every walk over saved history goes through here. The walk runs on a separate map so
    /// older turns cannot evict newer ownership, such as a turn's typed owner recorded from
    /// events that left the buffer. Items this map already recorded keep their owners, and
    /// this map's state is applied last as the newest entries.
    pub(crate) fn history_visibility<'a>(
        &mut self,
        turns: impl IntoIterator<Item = &'a Turn>,
    ) -> Vec<Vec<bool>> {
        let mut walk = Self::default();
        let visibility = turns
            .into_iter()
            .map(|turn| {
                walk.copy_turn_from(self, &turn.id);
                walk.saved_item_visibility(turn)
            })
            .collect();
        walk.overlay(self);
        *self = walk;
        visibility
    }

    /// Copies `other`'s ownership of `turn_id`, when it has any, as this map's newest turn.
    fn copy_turn_from(&mut self, other: &Self, turn_id: &str) {
        if let Some(turn) = other.turn(turn_id) {
            self.insert_newest(turn_id.to_string(), turn.clone());
        }
    }

    /// Moves every turn of `newer` behind this map's turns, so capacity eviction drops the
    /// older ones first. `newer`'s owners win; item owners only this map recorded are kept.
    fn overlay(&mut self, newer: &Self) {
        for (turn_id, newer_turn) in &newer.turns {
            let mut turn = match self.turns.iter().position(|(id, _)| id == turn_id) {
                Some(index) => self
                    .turns
                    .remove(index)
                    .map(|(_, turn)| turn)
                    .unwrap_or_else(|| newer_turn.clone()),
                None => newer_turn.clone(),
            };
            turn.owner = newer_turn.owner;
            turn.items.extend(
                newer_turn
                    .items
                    .iter()
                    .map(|(item_id, owner)| (item_id.clone(), *owner)),
            );
            self.insert_newest(turn_id.clone(), turn);
        }
    }

    fn insert_newest(&mut self, turn_id: String, turn: TurnOwners) {
        self.turns.retain(|(id, _)| *id != turn_id);
        if self.turns.len() == MAX_TRACKED_TURNS {
            self.turns.pop_front();
        }
        self.turns.push_back((turn_id, turn));
    }

    pub(crate) fn forget_turn(&mut self, turn_id: &str) {
        self.turns.retain(|(id, _)| id != turn_id);
    }
}
