use {
    criterion::{criterion_group, criterion_main, Bencher, Criterion},
    real_time::writable,
    std::{hint::black_box, thread},
};

fn writer(bencher: &mut Bencher) {
    let (writer, reader) = writable(0);

    thread::spawn({
        move || loop {
            let value = reader.get();
            black_box(value);
        }
    });

    bencher.iter(|| writer.set(1));
}

fn reader(bencher: &mut Bencher) {
    let (writer, reader) = writable(0);

    thread::spawn({
        move || loop {
            writer.set(1);
        }
    });

    bencher.iter(|| {
        let value = reader.get();
        black_box(value);
    });
}

fn reader_by_ref(bencher: &mut Bencher) {
    let (writer, mut reader) = writable(0);

    thread::spawn({
        move || loop {
            writer.set(1);
        }
    });

    bencher.iter(|| {
        let value = reader.get_ref();
        black_box(value);
    });
}

fn realtime_writer_benchmark(c: &mut Criterion) {
    c.bench_function("writer", writer);
    c.bench_function("reader", reader);
    c.bench_function("reader_by_ref", reader_by_ref);
}

criterion_group!(benches, realtime_writer_benchmark);
criterion_main!(benches);
