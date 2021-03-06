use crate::{CarID, CarStatus, DrawCarInput, Event, ParkedCar, ParkingSpot, PersonID, Vehicle};
use abstutil::{
    deserialize_btreemap, deserialize_multimap, serialize_btreemap, serialize_multimap, MultiMap,
    Timer,
};
use geom::{Distance, PolyLine, Pt2D};
use map_model::{
    BuildingID, Lane, LaneID, LaneType, Map, ParkingLotID, PathConstraints, PathStep, Position,
    Traversable, TurnID,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap};

#[derive(Serialize, Deserialize, PartialEq, Clone)]
pub struct ParkingSimState {
    #[serde(
        serialize_with = "serialize_btreemap",
        deserialize_with = "deserialize_btreemap"
    )]
    parked_cars: BTreeMap<CarID, ParkedCar>,
    #[serde(
        serialize_with = "serialize_btreemap",
        deserialize_with = "deserialize_btreemap"
    )]
    occupants: BTreeMap<ParkingSpot, CarID>,
    reserved_spots: BTreeSet<ParkingSpot>,

    // On-street
    onstreet_lanes: BTreeMap<LaneID, ParkingLane>,
    // TODO Really this could be 0, 1, or 2 lanes. Full MultiMap is overkill.
    #[serde(
        serialize_with = "serialize_multimap",
        deserialize_with = "deserialize_multimap"
    )]
    driving_to_parking_lanes: MultiMap<LaneID, LaneID>,

    // Off-street
    num_spots_per_offstreet: BTreeMap<BuildingID, usize>,
    #[serde(
        serialize_with = "serialize_multimap",
        deserialize_with = "deserialize_multimap"
    )]
    driving_to_offstreet: MultiMap<LaneID, BuildingID>,

    // Parking lots
    num_spots_per_lot: BTreeMap<ParkingLotID, usize>,
    #[serde(
        serialize_with = "serialize_multimap",
        deserialize_with = "deserialize_multimap"
    )]
    driving_to_lots: MultiMap<LaneID, ParkingLotID>,

    events: Vec<Event>,
}

impl ParkingSimState {
    // Counterintuitive: any spots located in blackholes are just not represented here. If somebody
    // tries to drive from a blackholed spot, they couldn't reach most places.
    pub fn new(map: &Map, timer: &mut Timer) -> ParkingSimState {
        let mut sim = ParkingSimState {
            parked_cars: BTreeMap::new(),
            occupants: BTreeMap::new(),
            reserved_spots: BTreeSet::new(),

            onstreet_lanes: BTreeMap::new(),
            driving_to_parking_lanes: MultiMap::new(),
            num_spots_per_offstreet: BTreeMap::new(),
            driving_to_offstreet: MultiMap::new(),
            num_spots_per_lot: BTreeMap::new(),
            driving_to_lots: MultiMap::new(),

            events: Vec::new(),
        };
        for l in map.all_lanes() {
            if let Some(lane) = ParkingLane::new(l, map, timer) {
                sim.driving_to_parking_lanes.insert(lane.driving_lane, l.id);
                sim.onstreet_lanes.insert(lane.parking_lane, lane);
            }
        }
        for b in map.all_buildings() {
            if let Some(ref p) = b.parking {
                if map.get_l(p.driving_pos.lane()).parking_blackhole.is_none() {
                    sim.num_spots_per_offstreet.insert(b.id, p.num_spots);
                    sim.driving_to_offstreet.insert(p.driving_pos.lane(), b.id);
                }
            }
        }
        for pl in map.all_parking_lots() {
            // TODO Parking lots without any spots shouldn't be possible
            if pl.spots.is_empty() {
                continue;
            }
            if map.get_l(pl.driving_pos.lane()).parking_blackhole.is_none() {
                sim.num_spots_per_lot.insert(pl.id, pl.spots.len());
                sim.driving_to_lots.insert(pl.driving_pos.lane(), pl.id);
            }
        }
        sim
    }

    pub fn get_free_onstreet_spots(&self, l: LaneID) -> Vec<ParkingSpot> {
        let mut spots: Vec<ParkingSpot> = Vec::new();
        if let Some(lane) = self.onstreet_lanes.get(&l) {
            for spot in lane.spots() {
                if self.is_free(spot) {
                    spots.push(spot);
                }
            }
        }
        spots
    }

    pub fn get_free_offstreet_spots(&self, b: BuildingID) -> Vec<ParkingSpot> {
        let mut spots: Vec<ParkingSpot> = Vec::new();
        for idx in 0..self.num_spots_per_offstreet.get(&b).cloned().unwrap_or(0) {
            let spot = ParkingSpot::Offstreet(b, idx);
            if self.is_free(spot) {
                spots.push(spot);
            }
        }
        spots
    }

    pub fn get_free_lot_spots(&self, pl: ParkingLotID) -> Vec<ParkingSpot> {
        let mut spots: Vec<ParkingSpot> = Vec::new();
        for idx in 0..self.num_spots_per_lot.get(&pl).cloned().unwrap_or(0) {
            let spot = ParkingSpot::Lot(pl, idx);
            if self.is_free(spot) {
                spots.push(spot);
            }
        }
        spots
    }

    pub fn reserve_spot(&mut self, spot: ParkingSpot) {
        assert!(self.is_free(spot));
        self.reserved_spots.insert(spot);

        // Sanity check the spot exists
        match spot {
            ParkingSpot::Onstreet(l, idx) => {
                assert!(idx < self.onstreet_lanes[&l].spot_dist_along.len());
            }
            ParkingSpot::Offstreet(b, idx) => {
                assert!(idx < self.num_spots_per_offstreet[&b]);
            }
            ParkingSpot::Lot(pl, idx) => {
                assert!(idx < self.num_spots_per_lot[&pl]);
            }
        }
    }

    pub fn remove_parked_car(&mut self, p: ParkedCar) {
        self.parked_cars
            .remove(&p.vehicle.id)
            .expect("remove_parked_car missing from parked_cars");
        self.occupants
            .remove(&p.spot)
            .expect("remove_parked_car missing from occupants");
        self.events
            .push(Event::CarLeftParkingSpot(p.vehicle.id, p.spot));
    }

    pub fn add_parked_car(&mut self, p: ParkedCar) {
        self.events
            .push(Event::CarReachedParkingSpot(p.vehicle.id, p.spot));

        assert!(self.reserved_spots.remove(&p.spot));

        assert!(!self.occupants.contains_key(&p.spot));
        self.occupants.insert(p.spot, p.vehicle.id);

        assert!(!self.parked_cars.contains_key(&p.vehicle.id));
        self.parked_cars.insert(p.vehicle.id, p);
    }

    pub fn get_draw_cars(&self, id: LaneID, map: &Map) -> Vec<DrawCarInput> {
        let mut cars = Vec::new();
        if let Some(ref lane) = self.onstreet_lanes.get(&id) {
            for spot in lane.spots() {
                if let Some(car) = self.occupants.get(&spot) {
                    cars.push(self.get_draw_car(*car, map).unwrap());
                }
            }
        }
        cars
    }

    pub fn get_draw_cars_in_lots(&self, id: LaneID, map: &Map) -> Vec<DrawCarInput> {
        let mut cars = Vec::new();
        for pl in self.driving_to_lots.get(id) {
            for idx in 0..self.num_spots_per_lot[&pl] {
                if let Some(car) = self.occupants.get(&ParkingSpot::Lot(*pl, idx)) {
                    cars.push(self.get_draw_car(*car, map).unwrap());
                }
            }
        }
        cars
    }

    pub fn get_draw_car(&self, id: CarID, map: &Map) -> Option<DrawCarInput> {
        let p = self.parked_cars.get(&id)?;
        match p.spot {
            ParkingSpot::Onstreet(lane, idx) => {
                let front_dist = self.onstreet_lanes[&lane].dist_along_for_car(idx, &p.vehicle);
                Some(DrawCarInput {
                    id: p.vehicle.id,
                    waiting_for_turn: None,
                    status: CarStatus::Parked,
                    on: Traversable::Lane(lane),
                    label: None,

                    body: map
                        .get_l(lane)
                        .lane_center_pts
                        .exact_slice(front_dist - p.vehicle.length, front_dist),
                })
            }
            ParkingSpot::Offstreet(_, _) => None,
            ParkingSpot::Lot(pl, idx) => {
                let pl = map.get_pl(pl);
                let (pt, angle) = pl.spots[idx];
                let buffer = Distance::meters(0.5);
                Some(DrawCarInput {
                    id: p.vehicle.id,
                    waiting_for_turn: None,
                    status: CarStatus::Parked,
                    // Just used for z-order
                    on: Traversable::Lane(pl.driving_pos.lane()),
                    label: None,

                    body: PolyLine::new(vec![
                        pt.project_away(buffer, angle),
                        pt.project_away(map_model::PARKING_LOT_SPOT_LENGTH - buffer, angle),
                    ]),
                })
            }
        }
    }

    // There's no DrawCarInput for cars parked offstreet, so we need this.
    pub fn canonical_pt(&self, id: CarID, map: &Map) -> Option<Pt2D> {
        let p = self.parked_cars.get(&id)?;
        match p.spot {
            ParkingSpot::Onstreet(_, _) | ParkingSpot::Lot(_, _) => {
                self.get_draw_car(id, map).map(|c| c.body.last_pt())
            }
            ParkingSpot::Offstreet(b, _) => Some(map.get_b(b).label_center),
        }
    }

    pub fn get_all_draw_cars(&self, map: &Map) -> Vec<DrawCarInput> {
        self.parked_cars
            .keys()
            .filter_map(|id| self.get_draw_car(*id, map))
            .collect()
    }

    pub fn is_free(&self, spot: ParkingSpot) -> bool {
        !self.occupants.contains_key(&spot) && !self.reserved_spots.contains(&spot)
    }

    pub fn get_car_at_spot(&self, spot: ParkingSpot) -> Option<&ParkedCar> {
        let car = self.occupants.get(&spot)?;
        Some(&self.parked_cars[&car])
    }

    // The vehicle's front is currently at the given driving_pos. Returns all valid spots and their
    // driving position.
    pub fn get_all_free_spots(
        &self,
        driving_pos: Position,
        vehicle: &Vehicle,
        // Either the building where a seeded car starts or the target of a trip. For filtering
        // private spots.
        target: BuildingID,
        map: &Map,
    ) -> Vec<(ParkingSpot, Position)> {
        let mut candidates = Vec::new();

        for l in self.driving_to_parking_lanes.get(driving_pos.lane()) {
            let parking_dist = driving_pos
                .equiv_pos(*l, driving_pos.dist_along(), map)
                .dist_along();
            let lane = &self.onstreet_lanes[l];
            // Bit hacky to enumerate here to conveniently get idx.
            for (idx, spot) in lane.spots().into_iter().enumerate() {
                if self.is_free(spot) && parking_dist < lane.dist_along_for_car(idx, vehicle) {
                    candidates.push(spot);
                }
            }
        }

        for b in self.driving_to_offstreet.get(driving_pos.lane()) {
            let parking = map.get_b(*b).parking.as_ref().unwrap();
            if parking.public_garage_name.is_none() && target != *b {
                continue;
            }
            let bldg_dist = parking.driving_pos.dist_along();
            if driving_pos.dist_along() < bldg_dist {
                for idx in 0..self.num_spots_per_offstreet[b] {
                    let spot = ParkingSpot::Offstreet(*b, idx);
                    if self.is_free(spot) {
                        candidates.push(spot);
                    }
                }
            }
        }

        for pl in self.driving_to_lots.get(driving_pos.lane()) {
            let lot_dist = map.get_pl(*pl).driving_pos.dist_along();
            if driving_pos.dist_along() < lot_dist {
                for idx in 0..self.num_spots_per_lot[&pl] {
                    let spot = ParkingSpot::Lot(*pl, idx);
                    if self.is_free(spot) {
                        candidates.push(spot);
                    }
                }
            }
        }

        candidates
            .into_iter()
            .map(|spot| (spot, self.spot_to_driving_pos(spot, vehicle, map)))
            .collect()
    }

    pub fn spot_to_driving_pos(&self, spot: ParkingSpot, vehicle: &Vehicle, map: &Map) -> Position {
        match spot {
            ParkingSpot::Onstreet(l, idx) => {
                let lane = &self.onstreet_lanes[&l];
                Position::new(l, lane.dist_along_for_car(idx, vehicle)).equiv_pos(
                    lane.driving_lane,
                    vehicle.length,
                    map,
                )
            }
            ParkingSpot::Offstreet(b, _) => map.get_b(b).parking.as_ref().unwrap().driving_pos,
            ParkingSpot::Lot(pl, _) => map.get_pl(pl).driving_pos,
        }
    }

    pub fn spot_to_sidewalk_pos(&self, spot: ParkingSpot, map: &Map) -> Position {
        match spot {
            ParkingSpot::Onstreet(l, idx) => {
                let lane = &self.onstreet_lanes[&l];
                // Always centered in the entire parking spot
                Position::new(
                    l,
                    lane.spot_dist_along[idx] - (map_model::PARKING_SPOT_LENGTH / 2.0),
                )
                .equiv_pos(lane.sidewalk, Distance::ZERO, map)
            }
            ParkingSpot::Offstreet(b, _) => map.get_b(b).front_path.sidewalk,
            ParkingSpot::Lot(pl, _) => map.get_pl(pl).sidewalk_pos,
        }
    }

    pub fn get_owner_of_car(&self, id: CarID) -> Option<PersonID> {
        self.parked_cars.get(&id).and_then(|p| p.vehicle.owner)
    }
    pub fn lookup_parked_car(&self, id: CarID) -> Option<&ParkedCar> {
        self.parked_cars.get(&id)
    }

    // (Filled, available)
    pub fn get_all_parking_spots(&self) -> (Vec<ParkingSpot>, Vec<ParkingSpot>) {
        let mut spots = Vec::new();
        for lane in self.onstreet_lanes.values() {
            spots.extend(lane.spots());
        }
        for (b, num_spots) in &self.num_spots_per_offstreet {
            for idx in 0..*num_spots {
                spots.push(ParkingSpot::Offstreet(*b, idx));
            }
        }
        for (pl, num_spots) in &self.num_spots_per_lot {
            for idx in 0..*num_spots {
                spots.push(ParkingSpot::Lot(*pl, idx));
            }
        }

        let mut filled = Vec::new();
        let mut available = Vec::new();
        for spot in spots {
            if self.is_free(spot) {
                available.push(spot);
            } else {
                filled.push(spot);
            }
        }
        (filled, available)
    }

    // Unrealistically assumes the driver has knowledge of currently free parking spots, even if
    // they're far away. Since they don't reserve the spot in advance, somebody else can still beat
    // them there, producing some nice, realistic churn if there's too much contention.
    // The first PathStep is the turn after start, NOT PathStep::Lane(start).
    pub fn path_to_free_parking_spot(
        &self,
        start: LaneID,
        vehicle: &Vehicle,
        target: BuildingID,
        map: &Map,
    ) -> Option<(Vec<PathStep>, ParkingSpot, Position)> {
        let mut backrefs: HashMap<LaneID, TurnID> = HashMap::new();
        // Don't travel far.
        // This is a max-heap, so negate all distances. Tie breaker is lane ID, arbitrary but
        // deterministic.
        let mut queue: BinaryHeap<(Distance, LaneID)> = BinaryHeap::new();
        queue.push((Distance::ZERO, start));

        while !queue.is_empty() {
            let (dist_so_far, current) = queue.pop().unwrap();
            // If the current lane has a spot open, we wouldn't be asking. This can happen if a spot
            // opens up on the 'start' lane, but behind the car.
            if current != start {
                // Pick the closest to the start of the lane, since that's closest to where we came
                // from
                if let Some((spot, pos)) = self
                    .get_all_free_spots(
                        Position::new(current, Distance::ZERO),
                        vehicle,
                        target,
                        map,
                    )
                    .into_iter()
                    .min_by_key(|(_, pos)| pos.dist_along())
                {
                    let mut steps = vec![PathStep::Lane(current)];
                    let mut current = current;
                    loop {
                        if current == start {
                            // Don't include PathStep::Lane(start)
                            steps.pop();
                            steps.reverse();
                            return Some((steps, spot, pos));
                        }
                        let turn = backrefs[&current];
                        steps.push(PathStep::Turn(turn));
                        steps.push(PathStep::Lane(turn.src));
                        current = turn.src;
                    }
                }
            }
            for turn in map.get_turns_for(current, PathConstraints::Car) {
                if !backrefs.contains_key(&turn.id.dst) {
                    let dist_this_step = turn.geom.length() + map.get_l(current).length();
                    backrefs.insert(turn.id.dst, turn.id);
                    // Remember, keep things negative
                    queue.push((dist_so_far - dist_this_step, turn.id.dst));
                }
            }
        }

        None
    }

    pub fn collect_events(&mut self) -> Vec<Event> {
        std::mem::replace(&mut self.events, Vec::new())
    }
}

#[derive(Serialize, Deserialize, PartialEq, Clone)]
struct ParkingLane {
    parking_lane: LaneID,
    driving_lane: LaneID,
    sidewalk: LaneID,
    // The front of the parking spot (farthest along the lane)
    spot_dist_along: Vec<Distance>,
}

impl ParkingLane {
    fn new(lane: &Lane, map: &Map, timer: &mut Timer) -> Option<ParkingLane> {
        if lane.lane_type != LaneType::Parking {
            return None;
        }

        let driving_lane = if let Some(l) = map.get_parent(lane.id).parking_to_driving(lane.id) {
            l
        } else {
            // Serious enough to blow up loudly.
            panic!("Parking lane {} has no driving lane!", lane.id);
        };
        if map.get_l(driving_lane).parking_blackhole.is_some() {
            return None;
        }
        let sidewalk = if let Ok(l) = map.find_closest_lane(lane.id, vec![LaneType::Sidewalk]) {
            l
        } else {
            timer.warn(format!("Parking lane {} has no sidewalk!", lane.id));
            return None;
        };

        Some(ParkingLane {
            parking_lane: lane.id,
            driving_lane,
            sidewalk,
            spot_dist_along: (0..lane.number_parking_spots())
                .map(|idx| map_model::PARKING_SPOT_LENGTH * (2.0 + idx as f64))
                .collect(),
        })
    }

    fn dist_along_for_car(&self, spot_idx: usize, vehicle: &Vehicle) -> Distance {
        // Find the offset to center this particular car in the parking spot
        self.spot_dist_along[spot_idx] - (map_model::PARKING_SPOT_LENGTH - vehicle.length) / 2.0
    }

    fn spots(&self) -> Vec<ParkingSpot> {
        let mut spots = Vec::new();
        for idx in 0..self.spot_dist_along.len() {
            spots.push(ParkingSpot::Onstreet(self.parking_lane, idx));
        }
        spots
    }
}
