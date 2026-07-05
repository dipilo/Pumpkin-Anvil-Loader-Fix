use super::{Controls, Goal, GoalFuture, to_goal_ticks};
use crate::entity::{
    ai::{pathfinder::NavigatorGoal, pathfinder::pathfinding_context::PathfindingContext},
    mob::Mob,
};
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;

const HORIZONTAL_RANGE: i32 = 10;
const VERTICAL_RANGE: i32 = 7;
const MAX_CANDIDATES: usize = 10;

pub struct WanderAroundGoal {
    goal_control: Controls,
    speed: f64,
    target: Option<Vector3<f64>>,
    chance: i32,
}

impl WanderAroundGoal {
    #[must_use]
    pub const fn new(speed: f64) -> Self {
        Self {
            goal_control: Controls::MOVE,
            speed,
            target: None,
            chance: to_goal_ticks(120),
        }
    }

    fn find_wander_target(mob: &dyn Mob) -> Option<Vector3<f64>> {
        let entity = &mob.get_mob_entity().living_entity.entity;
        let origin = entity.pos.load().to_i32();
        let world = entity.world.load_full();
        let mut context = PathfindingContext::new(origin, world);
        let mut rng = mob.get_random();

        for _ in 0..MAX_CANDIDATES {
            let dx = rng.random_range(-HORIZONTAL_RANGE..=HORIZONTAL_RANGE);
            let dy = rng.random_range(-VERTICAL_RANGE..=VERTICAL_RANGE);
            let dz = rng.random_range(-HORIZONTAL_RANGE..=HORIZONTAL_RANGE);
            let candidate = Vector3::new(origin.x + dx, origin.y + dy, origin.z + dz);

            let path_type = context.get_land_node_type(candidate);
            if path_type.get_malus() != 0.0 {
                continue;
            }

            return Some(Vector3::new(
                f64::from(candidate.x) + 0.5,
                f64::from(candidate.y),
                f64::from(candidate.z) + 0.5,
            ));
        }

        None
    }
}

impl Goal for WanderAroundGoal {
    fn can_start<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async move {
            if mob.get_random().random_range(0..self.chance) != 0 {
                return false;
            }

            let target = Self::find_wander_target(mob);
            self.target = target;
            self.target.is_some()
        })
    }

    fn should_continue<'a>(&'a self, mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async move {
            let navigator = mob.get_mob_entity().navigator.lock().unwrap();
            !navigator.is_idle()
        })
    }

    fn start<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            if let Some(target) = self.target {
                let pos = mob.get_mob_entity().living_entity.entity.pos.load();
                let mut navigator = mob.get_mob_entity().navigator.lock().unwrap();
                navigator.set_progress(NavigatorGoal::new(pos, target, self.speed));
            }
        })
    }

    fn stop<'a>(&'a mut self, _mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            self.target = None;
        })
    }

    fn controls(&self) -> Controls {
        self.goal_control
    }
}
