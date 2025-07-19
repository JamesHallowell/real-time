use {
    criterion::{criterion_group, criterion_main, Bencher, Criterion},
    real_time::readable,
    std::{hint::black_box, thread},
};

fn writer(bencher: &mut Bencher) {
    let (writer, mut reader) = readable(0);

    thread::spawn({
        move || loop {
            let value = reader.read();
            black_box(*value);
        }
    });

    bencher.iter(|| writer.set(1));
}

fn reader(bencher: &mut Bencher) {
    let (writer, mut reader) = readable(0);

    thread::spawn({
        move || loop {
            writer.set(1);
        }
    });

    bencher.iter(|| {
        let value = reader.read();
        black_box(*value);
    });
}

fn realtime_reader_benchmark(c: &mut Criterion) {
    c.bench_function("writer", writer);
    c.bench_function("reader", reader);
}

criterion_group!(benches, realtime_reader_benchmark);
criterion_main!(benches);
