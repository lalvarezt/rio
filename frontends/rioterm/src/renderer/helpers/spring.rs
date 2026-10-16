#[derive(Clone)]
pub struct Spring {
    pub position: f32,
    pub velocity: f32,
}

impl Spring {
    #[inline]
    pub fn new() -> Self {
        Self {
            position: 0.0,
            velocity: 0.0,
        }
    }

    #[inline]
    pub fn reset(&mut self) {
        self.position = 0.0;
        self.velocity = 0.0;
    }

    /// Header motion settles within 2% in `settle_time`, then stops
    /// once both displacement and velocity are below a physical-pixel budget.
    pub fn settle(&mut self, dt: f32, settle_time: f32, scale: f32) -> bool {
        if self.position == 0.0 && self.velocity == 0.0 {
            return false;
        }
        let omega = 6.0 / settle_time;
        let a = self.position;
        let b = a * omega + self.velocity;
        let c = (-omega * dt).exp();
        self.position = (a + b * dt) * c;
        self.velocity = (self.velocity - b * omega * dt) * c;
        let tolerance = 0.25 / scale;
        if self.position.abs() <= tolerance && self.velocity.abs() <= tolerance * omega {
            self.reset();
            false
        } else {
            true
        }
    }
}
