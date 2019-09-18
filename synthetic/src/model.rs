use abstutil::{read_binary, MultiMap, Timer};
use ezgui::world::{Object, ObjectID, World};
use ezgui::{Color, EventCtx, GfxCtx, Line, Prerender, Text};
use geom::{Bounds, Circle, Distance, PolyLine, Polygon, Pt2D};
use map_model::raw_data::{
    MapFixes, OriginalIntersection, OriginalRoad, StableBuildingID, StableIntersectionID,
    StableRoadID,
};
use map_model::{osm, raw_data, IntersectionType, LaneType, RoadSpec, LANE_THICKNESS};
use std::collections::{BTreeMap, BTreeSet};
use std::mem;

const INTERSECTION_RADIUS: Distance = Distance::const_meters(5.0);
const BUILDING_LENGTH: Distance = Distance::const_meters(30.0);
const CENTER_LINE_THICKNESS: Distance = Distance::const_meters(0.5);

const SYNTHETIC_OSM_WAY_ID: i64 = -1;

pub type Direction = bool;
const FORWARDS: Direction = true;
const BACKWARDS: Direction = false;

pub struct Model {
    pub map: raw_data::Map,
    roads_per_intersection: MultiMap<StableIntersectionID, StableRoadID>,
    // Never reuse IDs, and don't worry about being sequential
    id_counter: usize,
    all_fixes: BTreeMap<String, MapFixes>,

    exclude_bldgs: bool,
    edit_fixes: Option<String>,
    world: World<ID>,
}

// Construction
impl Model {
    pub fn blank() -> Model {
        Model {
            map: raw_data::Map::blank(String::new()),
            roads_per_intersection: MultiMap::new(),
            id_counter: 0,
            all_fixes: BTreeMap::new(),

            exclude_bldgs: false,
            edit_fixes: None,
            world: World::new(&Bounds::new()),
        }
    }

    pub fn import(
        path: &str,
        exclude_bldgs: bool,
        edit_fixes: Option<String>,
        prerender: &Prerender,
    ) -> Model {
        let mut timer = Timer::new("import map");
        let mut model = Model::blank();
        model.all_fixes = MapFixes::load(&mut timer);
        model.exclude_bldgs = exclude_bldgs;
        model.edit_fixes = edit_fixes;
        model.map = read_binary(path, &mut timer).unwrap();
        model.map.apply_fixes(&model.all_fixes, &mut timer);

        if let Some(ref name) = model.edit_fixes {
            if !model.all_fixes.contains_key(name) {
                model.all_fixes.insert(
                    name.clone(),
                    MapFixes {
                        delete_roads: Vec::new(),
                        delete_intersections: Vec::new(),
                        add_intersections: Vec::new(),
                        add_roads: Vec::new(),
                    },
                );
            }
        }

        for id in model.map.buildings.keys() {
            model.id_counter = model.id_counter.max(id.0 + 1);
        }

        for id in model.map.intersections.keys() {
            model.id_counter = model.id_counter.max(id.0 + 1);
        }

        for (id, r) in &model.map.roads {
            model.id_counter = model.id_counter.max(id.0 + 1);

            model.roads_per_intersection.insert(r.i1, *id);
            model.roads_per_intersection.insert(r.i2, *id);
        }

        model.world = World::new(&model.compute_bounds());
        if !model.exclude_bldgs {
            for id in model.map.buildings.keys().cloned().collect::<Vec<_>>() {
                model.bldg_added(id, prerender);
            }
        }
        for id in model.map.intersections.keys().cloned().collect::<Vec<_>>() {
            model.intersection_added(id, prerender);
        }
        for id in model.map.roads.keys().cloned().collect::<Vec<_>>() {
            model.road_added(id, prerender);
        }

        model
    }
}

// General
impl Model {
    pub fn draw(&self, g: &mut GfxCtx) {
        g.clear(Color::BLACK);
        g.draw_polygon(Color::rgb(242, 239, 233), &self.map.boundary_polygon);
        self.world.draw(g);
    }

    pub fn handle_mouseover(&mut self, ctx: &EventCtx) {
        self.world.handle_mouseover(ctx);
    }

    pub fn get_selection(&self) -> Option<ID> {
        self.world.get_selection()
    }

    // TODO Only for truly synthetic maps...
    pub fn export(&mut self) {
        assert!(self.map.name != "");
        // TODO Or maybe we should do this more regularly?
        self.map.boundary_polygon = self.compute_bounds().get_rectangle();

        let path = abstutil::path_raw_map(&self.map.name);
        abstutil::write_binary(&path, &self.map).expect(&format!("Saving {} failed", path));
        println!("Exported {}", path);
    }

    pub fn save_fixes(&mut self) {
        let name = if let Some(ref n) = self.edit_fixes {
            n.clone()
        } else {
            println!("Not editing any fixes, so can't save them");
            return;
        };
        let mut fixes = self.all_fixes.remove(&name).unwrap();

        // It's easiest to just go back and detect all of the added roads and intersections. But we
        // have to avoid picking up changes from other fixes.
        // TODO Ideally fixes would have a Polygon of where they influence, and all of the polygons
        // would be disjoint. Nothing prevents fixes from being saved in the wrong group, or a
        // created road from one set to be deleted in another -- we're just sure that a fix isn't
        // repeated.
        let mut ignore_roads: BTreeSet<OriginalRoad> = BTreeSet::new();
        let mut ignore_intersections: BTreeSet<OriginalIntersection> = BTreeSet::new();
        for f in self.all_fixes.values() {
            let (r, i) = f.all_touched_ids();
            ignore_roads.extend(r);
            ignore_intersections.extend(i);
        }

        fixes.add_intersections.clear();
        fixes.add_roads.clear();
        for i in self.map.intersections.values() {
            if i.synthetic && !ignore_intersections.contains(&i.orig_id) {
                fixes.add_intersections.push(i.clone());
            }
        }
        for r in self.map.roads.values() {
            if r.osm_tags.get(osm::SYNTHETIC) == Some(&"true".to_string())
                && !ignore_roads.contains(&r.orig_id)
            {
                fixes.add_roads.push(r.clone());
            }
        }

        let path = abstutil::path_fixes(&name);
        abstutil::write_binary(&path, &fixes).unwrap();
        println!("Wrote {}", path);

        self.all_fixes.insert(name, fixes);
    }

    fn compute_bounds(&self) -> Bounds {
        let mut bounds = Bounds::new();
        for b in self.map.buildings.values() {
            for pt in b.polygon.points() {
                bounds.update(*pt);
            }
        }
        for i in self.map.intersections.values() {
            bounds.update(i.point);
        }
        for r in self.map.roads.values() {
            for pt in &r.center_points {
                bounds.update(*pt);
            }
        }
        bounds
    }

    pub fn delete_everything_inside(&mut self, area: Polygon) {
        if !self.exclude_bldgs {
            for id in self.map.buildings.keys().cloned().collect::<Vec<_>>() {
                if area.contains_pt(self.map.buildings[&id].polygon.center()) {
                    self.delete_b(id);
                }
            }
        }

        for id in self.map.roads.keys().cloned().collect::<Vec<_>>() {
            if self.map.roads[&id]
                .center_points
                .iter()
                .any(|pt| area.contains_pt(*pt))
            {
                self.delete_r(id);
            }
        }
        for id in self.map.intersections.keys().cloned().collect::<Vec<_>>() {
            if area.contains_pt(self.map.intersections[&id].point) {
                self.delete_i(id);
            }
        }
    }
}

// Intersections
impl Model {
    fn intersection_added(&mut self, id: StableIntersectionID, prerender: &Prerender) {
        let i = &self.map.intersections[&id];
        let color = match i.intersection_type {
            IntersectionType::TrafficSignal => Color::GREEN,
            IntersectionType::StopSign => Color::RED,
            IntersectionType::Border => Color::BLUE,
        };
        self.world.add(
            prerender,
            Object::new(
                ID::Intersection(id),
                if i.synthetic { color.alpha(0.5) } else { color },
                Circle::new(i.point, INTERSECTION_RADIUS).to_polygon(),
            )
            .maybe_label(i.label.clone()),
        );
    }

    pub fn create_i(&mut self, point: Pt2D, prerender: &Prerender) {
        let id = StableIntersectionID(self.id_counter);
        self.id_counter += 1;
        self.map.intersections.insert(
            id,
            raw_data::Intersection {
                point,
                intersection_type: IntersectionType::StopSign,
                label: None,
                orig_id: raw_data::OriginalIntersection {
                    point: point.forcibly_to_gps(&self.map.gps_bounds),
                },
                synthetic: true,
            },
        );

        self.intersection_added(id, prerender);
    }

    pub fn move_i(&mut self, id: StableIntersectionID, point: Pt2D, prerender: &Prerender) {
        self.world.delete(ID::Intersection(id));

        let gps_pt = {
            let i = self.map.intersections.get_mut(&id).unwrap();
            i.point = point;
            i.orig_id.point = point.forcibly_to_gps(&self.map.gps_bounds);
            i.orig_id.point
        };

        self.intersection_added(id, prerender);

        // Now update all the roads.
        for r in self.roads_per_intersection.get(id).clone() {
            self.road_deleted(r);

            let road = self.map.roads.get_mut(&r).unwrap();
            if road.i1 == id {
                road.center_points[0] = point;
                // TODO This is valid for synthetic roads, but maybe weird otherwise...
                road.orig_id.pt1 = gps_pt;
            } else {
                assert_eq!(road.i2, id);
                *road.center_points.last_mut().unwrap() = point;
                road.orig_id.pt2 = gps_pt;
            }

            self.road_added(r, prerender);
        }
    }

    pub fn set_i_label(&mut self, id: StableIntersectionID, label: String, prerender: &Prerender) {
        self.world.delete(ID::Intersection(id));

        self.map.intersections.get_mut(&id).unwrap().label = Some(label);

        self.intersection_added(id, prerender);
    }

    pub fn get_i_label(&self, id: StableIntersectionID) -> Option<String> {
        self.map.intersections[&id].label.clone()
    }

    pub fn toggle_i_type(&mut self, id: StableIntersectionID, prerender: &Prerender) {
        self.world.delete(ID::Intersection(id));

        let i = self.map.intersections.get_mut(&id).unwrap();
        i.intersection_type = match i.intersection_type {
            IntersectionType::StopSign => IntersectionType::TrafficSignal,
            IntersectionType::TrafficSignal => {
                if self.roads_per_intersection.get(id).len() == 1 {
                    IntersectionType::Border
                } else {
                    IntersectionType::StopSign
                }
            }
            IntersectionType::Border => IntersectionType::StopSign,
        };

        self.intersection_added(id, prerender);
    }

    pub fn delete_i(&mut self, id: StableIntersectionID) {
        if !self.roads_per_intersection.get(id).is_empty() {
            println!("Can't delete intersection used by roads");
            return;
        }
        let i = self.map.intersections.remove(&id).unwrap();

        self.world.delete(ID::Intersection(id));

        if let Some(ref name) = self.edit_fixes {
            if !i.synthetic {
                self.all_fixes
                    .get_mut(name)
                    .unwrap()
                    .delete_intersections
                    .push(i.orig_id);
            }
        } else {
            println!("This won't be saved in any MapFixes!");
        }
    }

    pub fn get_i_center(&self, id: StableIntersectionID) -> Pt2D {
        self.map.intersections[&id].point
    }
}

impl Model {
    fn road_added(&mut self, id: StableRoadID, prerender: &Prerender) {
        for obj in self.lanes(id) {
            self.world.add(prerender, obj);
        }
    }

    fn road_deleted(&mut self, id: StableRoadID) {
        for obj in self.lanes(id) {
            self.world.delete(obj.get_id());
        }
    }

    pub fn create_r(
        &mut self,
        i1: StableIntersectionID,
        i2: StableIntersectionID,
        prerender: &Prerender,
    ) {
        // Ban cul-de-sacs, since they get stripped out later anyway.
        if self
            .map
            .roads
            .values()
            .any(|r| (r.i1 == i1 && r.i2 == i2) || (r.i1 == i2 && r.i2 == i1))
        {
            println!("Road already exists");
            return;
        }

        let mut osm_tags = BTreeMap::new();
        osm_tags.insert(osm::SYNTHETIC.to_string(), "true".to_string());
        osm_tags.insert(
            osm::SYNTHETIC_LANES.to_string(),
            RoadSpec {
                fwd: vec![LaneType::Driving, LaneType::Parking, LaneType::Sidewalk],
                back: vec![LaneType::Driving, LaneType::Parking, LaneType::Sidewalk],
            }
            .to_string(),
        );
        osm_tags.insert(osm::ENDPT_FWD.to_string(), "true".to_string());
        osm_tags.insert(osm::ENDPT_BACK.to_string(), "true".to_string());
        osm_tags.insert(
            osm::OSM_WAY_ID.to_string(),
            SYNTHETIC_OSM_WAY_ID.to_string(),
        );
        // Reasonable defaults.
        osm_tags.insert(osm::NAME.to_string(), "Streety McStreetFace".to_string());
        osm_tags.insert(osm::MAXSPEED.to_string(), "25 mph".to_string());
        let center_points = vec![
            self.map.intersections[&i1].point,
            self.map.intersections[&i2].point,
        ];
        let id = StableRoadID(self.id_counter);
        self.id_counter += 1;
        self.map.roads.insert(
            id,
            raw_data::Road {
                i1,
                i2,
                orig_id: raw_data::OriginalRoad {
                    osm_way_id: SYNTHETIC_OSM_WAY_ID,
                    pt1: center_points[0].forcibly_to_gps(&self.map.gps_bounds),
                    pt2: center_points[1].forcibly_to_gps(&self.map.gps_bounds),
                },
                center_points,
                osm_tags,
                osm_way_id: SYNTHETIC_OSM_WAY_ID,
                parking_lane_fwd: false,
                parking_lane_back: false,
            },
        );
        self.roads_per_intersection.insert(i1, id);
        self.roads_per_intersection.insert(i2, id);

        self.road_added(id, prerender);
    }

    pub fn edit_lanes(&mut self, id: StableRoadID, spec: String, prerender: &Prerender) {
        self.road_deleted(id);

        if let Some(s) = RoadSpec::parse(spec.clone()) {
            self.map
                .roads
                .get_mut(&id)
                .unwrap()
                .osm_tags
                .insert(osm::SYNTHETIC_LANES.to_string(), s.to_string());
        } else {
            println!("Bad RoadSpec: {}", spec);
        }

        self.road_added(id, prerender);
    }

    pub fn swap_lanes(&mut self, id: StableRoadID, prerender: &Prerender) {
        self.road_deleted(id);

        let r = self.map.roads.get_mut(&id).unwrap();
        let mut lanes = r.get_spec();
        mem::swap(&mut lanes.fwd, &mut lanes.back);
        r.osm_tags
            .insert(osm::SYNTHETIC_LANES.to_string(), lanes.to_string());

        let fwd_label = r.osm_tags.remove(osm::FWD_LABEL);
        let back_label = r.osm_tags.remove(osm::BACK_LABEL);
        if let Some(l) = fwd_label {
            r.osm_tags.insert(osm::BACK_LABEL.to_string(), l);
        }
        if let Some(l) = back_label {
            r.osm_tags.insert(osm::FWD_LABEL.to_string(), l);
        }

        self.road_added(id, prerender);
    }

    pub fn set_r_label(
        &mut self,
        pair: (StableRoadID, Direction),
        label: String,
        prerender: &Prerender,
    ) {
        self.road_deleted(pair.0);

        let r = self.map.roads.get_mut(&pair.0).unwrap();
        if pair.1 {
            r.osm_tags
                .insert(osm::FWD_LABEL.to_string(), label.to_string());
        } else {
            r.osm_tags
                .insert(osm::BACK_LABEL.to_string(), label.to_string());
        }

        self.road_added(pair.0, prerender);
    }

    pub fn get_r_label(&self, pair: (StableRoadID, Direction)) -> Option<String> {
        let r = &self.map.roads[&pair.0];
        if pair.1 {
            r.osm_tags.get(osm::FWD_LABEL).cloned()
        } else {
            r.osm_tags.get(osm::BACK_LABEL).cloned()
        }
    }

    pub fn set_r_name_and_speed(
        &mut self,
        id: StableRoadID,
        name: String,
        speed: String,
        prerender: &Prerender,
    ) {
        self.road_deleted(id);

        let r = self.map.roads.get_mut(&id).unwrap();
        r.osm_tags.insert(osm::NAME.to_string(), name);
        r.osm_tags.insert(osm::MAXSPEED.to_string(), speed);

        self.road_added(id, prerender);
    }

    pub fn get_r_name_and_speed(&self, id: StableRoadID) -> (String, String) {
        let r = &self.map.roads[&id];
        (
            r.osm_tags
                .get(osm::NAME)
                .cloned()
                .unwrap_or_else(String::new),
            r.osm_tags
                .get(osm::MAXSPEED)
                .cloned()
                .unwrap_or_else(String::new),
        )
    }

    pub fn delete_r(&mut self, id: StableRoadID) {
        self.road_deleted(id);

        let r = self.map.roads.remove(&id).unwrap();
        self.roads_per_intersection.remove(r.i1, id);
        self.roads_per_intersection.remove(r.i2, id);

        if let Some(ref name) = self.edit_fixes {
            if r.osm_tags.get(osm::SYNTHETIC) != Some(&"true".to_string()) {
                self.all_fixes
                    .get_mut(name)
                    .unwrap()
                    .delete_roads
                    .push(r.orig_id);
            }
        } else {
            println!("This won't be saved in any MapFixes!");
        }
    }

    pub fn get_road_spec(&self, id: StableRoadID) -> String {
        self.map.roads[&id].get_spec().to_string()
    }

    pub fn get_tags(&self, id: StableRoadID) -> &BTreeMap<String, String> {
        &self.map.roads[&id].osm_tags
    }

    fn lanes(&self, id: StableRoadID) -> Vec<Object<ID>> {
        let r = &self.map.roads[&id];

        let mut tooltip = Text::new();
        if let Some(name) = r.osm_tags.get(osm::NAME) {
            tooltip.add(Line(name));
        } else if let Some(name) = r.osm_tags.get("ref") {
            tooltip.add(Line(name));
        } else {
            tooltip.add(Line("some road"));
        }

        let mut result = Vec::new();
        let synthetic = r.osm_tags.get(osm::SYNTHETIC) == Some(&"true".to_string());
        let spec = r.get_spec();
        let center_pts = PolyLine::new(r.center_points.clone());
        for (idx, lt) in spec.fwd.iter().enumerate() {
            let mut obj = Object::new(
                ID::Lane(id, FORWARDS, idx),
                Model::lt_to_color(*lt, synthetic),
                center_pts
                    .shift_right(LANE_THICKNESS * (0.5 + (idx as f64)))
                    .unwrap()
                    .make_polygons(LANE_THICKNESS),
            );
            if idx == 0 {
                obj = obj.push(
                    Color::YELLOW,
                    center_pts.make_polygons(CENTER_LINE_THICKNESS),
                );
            }
            if idx == spec.fwd.len() / 2 {
                obj = obj.maybe_label(r.osm_tags.get(osm::FWD_LABEL).cloned());
            }
            result.push(obj.tooltip(tooltip.clone()));
        }
        for (idx, lt) in spec.back.iter().enumerate() {
            let mut obj = Object::new(
                ID::Lane(id, BACKWARDS, idx),
                Model::lt_to_color(*lt, synthetic),
                center_pts
                    .reversed()
                    .shift_right(LANE_THICKNESS * (0.5 + (idx as f64)))
                    .unwrap()
                    .make_polygons(LANE_THICKNESS),
            );
            if idx == spec.back.len() / 2 {
                obj = obj.maybe_label(r.osm_tags.get(osm::BACK_LABEL).cloned());
            }
            result.push(obj.tooltip(tooltip.clone()));
        }

        result
    }

    // Copied from render/lane.rs. :(
    fn lt_to_color(lt: LaneType, synthetic: bool) -> Color {
        let color = match lt {
            LaneType::Driving => Color::BLACK,
            LaneType::Bus => Color::rgb(190, 74, 76),
            LaneType::Parking => Color::grey(0.2),
            LaneType::Sidewalk => Color::grey(0.8),
            LaneType::Biking => Color::rgb(15, 125, 75),
        };
        if synthetic {
            color.alpha(0.5)
        } else {
            color
        }
    }
}

impl Model {
    fn bldg_added(&mut self, id: StableBuildingID, prerender: &Prerender) {
        let b = &self.map.buildings[&id];
        self.world.add(
            prerender,
            Object::new(ID::Building(id), Color::BLUE, b.polygon.clone())
                .maybe_label(b.osm_tags.get(osm::LABEL).cloned()),
        );
    }

    pub fn create_b(&mut self, center: Pt2D, prerender: &Prerender) {
        let id = StableBuildingID(self.id_counter);
        self.id_counter += 1;
        self.map.buildings.insert(
            id,
            raw_data::Building {
                polygon: Polygon::rectangle(center, BUILDING_LENGTH, BUILDING_LENGTH),
                osm_tags: BTreeMap::new(),
                osm_way_id: SYNTHETIC_OSM_WAY_ID,
                parking: None,
            },
        );

        self.bldg_added(id, prerender);
    }

    pub fn move_b(&mut self, id: StableBuildingID, new_center: Pt2D, prerender: &Prerender) {
        self.world.delete(ID::Building(id));

        let b = self.map.buildings.get_mut(&id).unwrap();
        let old_center = b.polygon.center();
        b.polygon = b.polygon.translate(
            Distance::meters(new_center.x() - old_center.x()),
            Distance::meters(new_center.y() - old_center.y()),
        );

        self.bldg_added(id, prerender);
    }

    pub fn set_b_label(&mut self, id: StableBuildingID, label: String, prerender: &Prerender) {
        self.world.delete(ID::Building(id));

        self.map
            .buildings
            .get_mut(&id)
            .unwrap()
            .osm_tags
            .insert(osm::LABEL.to_string(), label);

        self.bldg_added(id, prerender);
    }

    pub fn get_b_label(&self, id: StableBuildingID) -> Option<String> {
        self.map.buildings[&id].osm_tags.get(osm::LABEL).cloned()
    }

    pub fn delete_b(&mut self, id: StableBuildingID) {
        self.world.delete(ID::Building(id));

        self.map.buildings.remove(&id);
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum ID {
    Building(StableBuildingID),
    Intersection(StableIntersectionID),
    Lane(StableRoadID, Direction, usize),
}

impl ObjectID for ID {
    fn zorder(&self) -> usize {
        match self {
            ID::Lane(_, _, _) => 0,
            ID::Intersection(_) => 1,
            ID::Building(_) => 2,
        }
    }
}