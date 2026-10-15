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

    /// Advance by variable `dt`. Returns `true` while still moving.
    #[inline]
    #[cfg(any(test, not(target_os = "windows")))]
    pub fn update(&mut self, dt: f32, animation_length: f32) -> bool {
        if animation_length <= dt {
            self.reset();
            return false;
        }
        if self.position == 0.0 {
            return false;
        }

        // Critically-damped spring (zeta = 1.0).
        // omega chosen so destination is reached within ~9% tolerance in
        // `animation_length` time.
        let omega = 4.0 / animation_length;

        // Analytical solution for critically-damped harmonic oscillation.
        let a = self.position;
        let b = a * omega + self.velocity;
        let c = (-omega * dt).exp();

        self.position = (a + b * dt) * c;
        self.velocity = c * (-a * omega - b * dt * omega + b);

        if self.position.abs() < 0.01 {
            self.reset();
            false
        } else {
            true
        }
    }

    /// Windows header motion settles within 2% in `settle_time`, then stops
    /// once both displacement and velocity are below a physical-pixel budget.
    #[cfg(target_os = "windows")]
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
