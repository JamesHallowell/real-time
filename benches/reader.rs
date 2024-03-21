use {
    criterion::{criterion_group, criterion_main, Bencher, Criterion},
    real_time::reader,
    std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        thread,
    },
};

fn writer(bencher: &mut Bencher) {
    let (mut writer, mut reader) = reader::realtime_reader(0);

    let stop = Arc::new(AtomicBool::new(false));
    let handle = thread::spawn({
        let stop = Arc::clone(&stop);
        move || loop {
            for _ in 0..1000 {
                let _ = reader.get();
            }
            if stop.load(Ordering::Relaxed) {
                break;
            }
        }
    });

    bencher.iter(|| writer.set(1));

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
}

fn reader(bencher: &mut Bencher) {
    let (mut writer, mut reader) = reader::realtime_reader(0);

    let stop = Arc::new(AtomicBool::new(false));
    let handle = thread::spawn({
        let stop = Arc::clone(&stop);
        move || loop {
            for _ in 0..1000 {
                let _ = writer.set(1);
            }
            if stop.load(Ordering::Relaxed) {
                break;
            }
        }
    });

    bencher.iter(|| {
        let _ = reader.get();
    });

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
}

fn realtime_reader_benchmark(c: &mut Criterion) {
    c.bench_function("writer", writer);
    c.bench_function("reader", reader);
}

criterion_group!(benches, realtime_reader_benchmark);
criterion_main!(benches);
