/// A simple widget for demonstrating Rust code chunking.
pub struct Widget {
    /// Internal value.
    x: i32,
}

impl Widget {
    /// Create a new Widget with a zero value.
    pub fn new() -> Self {
        Self { x: 0 }
    }

    /// Return the widget's current value.
    pub fn value(&self) -> i32 {
        self.x
    }

    /// Increment the widget's value by one.
    pub fn increment(&mut self) {
        self.x += 1;
    }
}

impl Default for Widget {
    fn default() -> Self {
        Self::new()
    }
}

/// A free function helper that returns a constant.
pub fn helper() -> i32 {
    1
}

/// Another helper that multiplies the value.
pub fn scale(v: i32, factor: i32) -> i32 {
    v * factor
}
