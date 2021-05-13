use lazy_static::lazy_static;
use std::sync::Mutex;

pub struct WaitForFreeCPU {
    num_cpus: usize,
    num_blocked: usize,
}

lazy_static! {
    static ref WAIT_FOR_FREE_CPU: Mutex<WaitForFreeCPU> = Mutex::new(WaitForFreeCPU {
        num_cpus: num_cpus::get(),
        num_blocked: 0,
    });
}

/// Waits for a CPU to be available and runs `f`. It is restricted
/// to only knowing about information within this process, but
/// should be sufficient to prevent spinning up too many CPU-heavy
/// processes in one go.
pub fn and_run<T>(f: impl FnOnce() -> T) -> T {
    loop {
        let mut w = WAIT_FOR_FREE_CPU.lock().unwrap();
        if w.num_blocked < w.num_cpus {
            w.num_blocked += 1;
            drop(w);
            let res = f();
            WAIT_FOR_FREE_CPU.lock().unwrap().num_blocked -= 1;
            return res;
        } else {
            drop(w);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}
