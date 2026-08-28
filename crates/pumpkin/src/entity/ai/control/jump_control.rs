use std::sync::atomic::Ordering;

use crate::entity::mob::Mob;

/// Vanilla-style one-shot jump request owner.
#[derive(Default)]
pub struct JumpControl {
    jump: bool,
}

impl JumpControl {
    pub const fn request_jump(&mut self) {
        self.jump = true;
    }

    fn take_request(&mut self) -> bool {
        std::mem::take(&mut self.jump)
    }

    pub fn tick(&mut self, mob: &dyn Mob) {
        mob.get_mob_entity()
            .living_entity
            .jumping
            .store(self.take_request(), Ordering::Relaxed);
    }

    pub const fn clear(&mut self) {
        self.jump = false;
    }
}

#[cfg(test)]
mod tests {
    use super::JumpControl;

    #[test]
    fn jump_request_is_consumed_exactly_once() {
        let mut control = JumpControl::default();
        assert!(!control.take_request());
        control.request_jump();
        assert!(control.take_request());
        assert!(!control.take_request());
    }
}
