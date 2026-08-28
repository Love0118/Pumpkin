use crate::entity::mob::Mob;
use pumpkin_util::math::clamp_angle;

const HEAD_STABLE_ANGLE: f32 = 15.0;
const FACE_FORWARD_DELAY: i32 = 10;
const FACE_FORWARD_DURATION: i32 = 10;
const MOVEMENT_EPSILON_SQUARED: f64 = 2.500_000_3E-7;

#[derive(Default)]
pub struct BodyRotationControl {
    head_stable_time: i32,
    last_stable_head_yaw: f32,
}

impl BodyRotationControl {
    fn update_angles(
        &mut self,
        moving: bool,
        carries_mob_passenger: bool,
        yaw: f32,
        mut head_yaw: f32,
        mut body_yaw: f32,
        max_head_rotation: f32,
    ) -> (f32, f32) {
        if moving {
            body_yaw = yaw;
            head_yaw = clamp_angle(head_yaw, body_yaw, max_head_rotation);
            self.last_stable_head_yaw = head_yaw;
            self.head_stable_time = 0;
        } else if !carries_mob_passenger {
            if (head_yaw - self.last_stable_head_yaw).abs() > HEAD_STABLE_ANGLE {
                self.head_stable_time = 0;
                self.last_stable_head_yaw = head_yaw;
                body_yaw = clamp_angle(body_yaw, head_yaw, max_head_rotation);
            } else {
                self.head_stable_time += 1;
                if self.head_stable_time > FACE_FORWARD_DELAY {
                    let elapsed = self.head_stable_time - FACE_FORWARD_DELAY;
                    let fraction = (elapsed as f32 / FACE_FORWARD_DURATION as f32).clamp(0.0, 1.0);
                    let remaining = max_head_rotation * (1.0 - fraction);
                    body_yaw = clamp_angle(body_yaw, head_yaw, remaining);
                }
            }
        }

        (head_yaw, body_yaw)
    }

    pub async fn tick(&mut self, mob: &dyn Mob) {
        let entity = &mob.get_mob_entity().living_entity.entity;
        let position = entity.pos.load();
        let previous = entity.last_pos.load();
        let dx = position.x - previous.x;
        let dz = position.z - previous.z;
        let moving = dx * dx + dz * dz > MOVEMENT_EPSILON_SQUARED;
        let carries_mob_passenger = entity
            .passengers
            .lock()
            .await
            .first()
            .is_some_and(|passenger| passenger.get_mob().is_some());
        let (head_yaw, body_yaw) = self.update_angles(
            moving,
            carries_mob_passenger,
            entity.yaw.load(),
            entity.head_yaw.load(),
            entity.body_yaw.load(),
            mob.get_max_head_rotation(),
        );
        entity.head_yaw.store(head_yaw);
        entity.body_yaw.store(body_yaw);
    }
}

#[cfg(test)]
mod tests {
    use super::BodyRotationControl;

    #[test]
    fn moving_body_faces_travel_yaw_and_clamps_head() {
        let mut control = BodyRotationControl::default();
        let (head, body) = control.update_angles(true, false, 90.0, -90.0, 0.0, 75.0);
        assert_eq!(body, 90.0);
        assert!((head - body).abs() <= 75.0);
    }

    #[test]
    fn stable_head_gradually_turns_body_forward_after_delay() {
        let mut control = BodyRotationControl::default();
        let mut body = 90.0;
        for _ in 0..20 {
            (_, body) = control.update_angles(false, false, 0.0, 0.0, body, 75.0);
        }
        assert_eq!(body, 0.0);
    }

    #[test]
    fn mob_passenger_suppresses_stationary_body_rotation() {
        let mut control = BodyRotationControl::default();
        let (_, body) = control.update_angles(false, true, 0.0, 90.0, 0.0, 75.0);
        assert_eq!(body, 0.0);
    }
}
