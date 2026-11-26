#[cfg(not(target_arch = "wasm32"))]
fn main() {
    use sashiko_worker_budget::{measure, native};
    let args: Vec<_> = std::env::args().collect();
    let mut executor = native(&args[1], args[2].parse().unwrap());
    println!(
        "{} {} scope {:?}",
        args[1],
        args[2],
        sashiko_worker_budget::verify(&mut executor)
    );
    for (name, heavy, physics, jobs, idle) in [
        ("light", false, true, true, false),
        ("contact", true, true, true, false),
        ("idle", true, true, true, true),
        ("physics", true, true, false, false),
        ("jobs", true, false, true, false),
    ] {
        let result = measure(&mut executor, heavy, physics, jobs, || {
            if idle {
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
        });
        println!("{} {} {name} {result:?}", args[1], args[2]);
    }
    for (name, late) in [("survivor-early", false), ("survivor-endgame", true)] {
        let result = sashiko_worker_budget::survivor_frame(&executor, late);
        println!("{} {} {name} {result:?}", args[1], args[2]);
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {}
