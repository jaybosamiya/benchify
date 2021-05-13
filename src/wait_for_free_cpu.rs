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

/// Set the max limit for number of "free CPUs" available. Will
/// automatically clamp to the total number of CPUs known to exist on
/// the system. Exists only to make the "free CPUs" estimate more
/// conservative.
pub fn restrict_free_cpus_to(n: usize) {
    let mut w = WAIT_FOR_FREE_CPU.lock().unwrap();

    // We can't ever let it go below the number that are currently
    // blocked, otherwise the expected invariant used by `and_run`
    // goes bad.
    let n = n.max(w.num_blocked);

    // We don't let it ever go above the number of CPUs known to exist
    // on the system. This is what we guarantee by contract of this
    // function.
    let n = n.min(num_cpus::get());

    // Set the value
    w.num_cpus = n;
}
