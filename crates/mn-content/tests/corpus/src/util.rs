/// Direction enum used in utility operations.
pub enum Direction {
    /// Move forward (positive).
    Forward,
    /// Move backward (negative).
    Backward,
    /// No movement.
    Neutral,
}

impl Direction {
    /// Return the sign associated with this direction.
    pub fn sign(&self) -> i32 {
        match self {
            Direction::Forward => 1,
            Direction::Backward => -1,
            Direction::Neutral => 0,
        }
    }
}

/// Clamp a value between a minimum and maximum.
pub fn clamp(value: i32, min: i32, max: i32) -> i32 {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

/// Compute the absolute difference between two integers.
pub fn abs_diff(a: i32, b: i32) -> i32 {
    (a - b).abs()
}
