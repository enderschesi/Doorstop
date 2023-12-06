use std::{
    ops::Mul,
    time::{Duration, Instant},
};

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Status {
    #[default]
    Idling,
    Queueing,
    Waiting,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Queue {
    status:        Status,
    #[allow(dead_code)]
    start_time:    Instant,
    current_place: Option<u32>,
    length:        Option<u32>,
    place_history: Vec<(u32, Instant)>,
}

impl Default for Queue {
    fn default() -> Self {
        Self {
            status:        Status::default(),
            start_time:    Instant::now(),
            current_place: Option::default(),
            length:        Option::default(),
            place_history: Vec::default(),
        }
    }
}

impl Queue {
    pub fn update_place(&mut self, current_place: u32) {
        if self.current_place.is_some() && self.current_place.unwrap() == current_place {
            return;
        }

        let current_time = Instant::now();
        self.current_place = Some(current_place);
        self.place_history.push((current_place, current_time));
    }

    pub const fn current_place(&self) -> Option<u32> {
        self.current_place
    }

    pub fn update_length(&mut self, length: u32) {
        self.length = Some(length);
    }

    pub const fn length(&self) -> Option<u32> {
        self.length
    }

    pub fn update_status(&mut self, status: Status) {
        self.status = status;
    }

    pub const fn status(&self) -> Status {
        self.status
    }

    pub fn estimate_time_till_end(&self) -> Option<Duration> {
        if self.place_history.len() < 2 || self.current_place.is_none() {
            return None;
        }

        let mut total_time = Duration::new(0, 0);
        let mut total_places = 0;

        for i in 1..self.place_history.len() {
            let (prev_place, prev_time) = self.place_history[i - 1];
            let (curr_place, curr_time) = self.place_history[i];

            let time_diff = curr_time.duration_since(prev_time);
            let place_diff = prev_place - curr_place;

            total_time += time_diff;
            total_places += place_diff;
        }

        let average_time_per_place = total_time.div_f64(f64::from(total_places));
        Some(average_time_per_place.mul(self.current_place.unwrap()))
    }
}
