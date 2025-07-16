use {
    criterion::{criterion_group, criterion_main, Bencher, Criterion},
    real_time::reader,
    std::{hint::black_box, thread},
};

fn writer(bencher: &mut Bencher) {
    let (writer, reader) = reader::realtime_reader(0);

    thread::spawn({
        move || loop {
            let value = reader.lock();
            black_box(*value);
        }
    });

    bencher.iter(|| writer.set(1));
}

fn reader(bencher: &mut Bencher) {
    let (writer, reader) = reader::realtime_reader(0);

    thread::spawn({
        move || loop {
            writer.set(1);
        }
    });

    bencher.iter(|| {
        let value = reader.lock();
        black_box(*value);
    });
}

fn realtime_reader_benchmark(c: &mut Criterion) {
    c.bench_function("writer", writer);
    c.bench_function("reader", reader);
}

criterion_group!(benches, realtime_reader_benchmark);
criterion_main!(benches);
