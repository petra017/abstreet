use dimensioned::si;
use map_model::{LaneID, Map, TurnID};
use std;
use std::collections::VecDeque;
use {Distance, On};

// This is all stuff that seems useful to share among different models.

// At all speeds (including at rest), cars must be at least this far apart.
pub const FOLLOWING_DISTANCE: Distance = si::Meter {
    value_unsafe: 8.0,
    _marker: std::marker::PhantomData,
};

// These might have slightly different meanings in different models...
pub(crate) enum Action {
    Vanish,      // done with route (and transitioning to a different state isn't implemented yet)
    Continue,    // need more time to cross the current spot
    Goto(On),    // go somewhere
    WaitFor(On), // ready to go somewhere, but can't yet for some reason
}

pub(crate) fn choose_turn(
    path: &VecDeque<LaneID>,
    waiting_for: &Option<On>,
    from: LaneID,
    map: &Map,
) -> TurnID {
    // TODO waiting_for check doesn't make sense for aorta driving model. what was the point of
    // this?
    assert!(waiting_for.is_none());
    for t in map.get_turns_from_lane(from) {
        if t.dst == path[0] {
            return t.id;
        }
    }
    panic!("No turn from {} to {}", from, path[0]);
}
