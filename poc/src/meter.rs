use std::time::Instant;

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

pub struct Meter {
    last_wall: Instant,
    last_cpu: u64,
}

impl Meter {
    pub fn new() -> Self {
        Self {
            last_wall: Instant::now(),
            last_cpu: process_cpu(),
        }
    }

    pub fn sample(&mut self) -> (f32, u64) {
        let now = Instant::now();
        let cpu = process_cpu();
        let wall = now.duration_since(self.last_wall).as_micros().max(1) as f32;
        let percent = ((cpu.saturating_sub(self.last_cpu)) as f32) / 10.0 / wall * 100.0;
        self.last_wall = now;
        self.last_cpu = cpu;
        (percent.clamp(0.0, 400.0), working_set())
    }
}

fn process_cpu() -> u64 {
    unsafe {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        if GetProcessTimes(GetCurrentProcess(), &mut creation, &mut exit, &mut kernel, &mut user).is_err() {
            return 0;
        }
        filetime(kernel) + filetime(user)
    }
}

fn filetime(value: FILETIME) -> u64 {
    ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
}

fn working_set() -> u64 {
    unsafe {
        let mut counters = PROCESS_MEMORY_COUNTERS::default();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            counters.cb,
        )
        .is_err()
        {
            return 0;
        }
        counters.WorkingSetSize as u64
    }
}
