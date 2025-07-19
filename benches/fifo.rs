use {
    criterion::{criterion_group, criterion_main, Bencher, Criterion},
    real_time::fifo,
    std::hint::black_box,
};

fn write(bencher: &mut Bencher) {
    let (writer, _reader) = fifo::<_, 128>();

    bencher.iter(|| writer.push(1));
}

fn read(bencher: &mut Bencher) {
    let (writer, reader) = fifo::<_, 128>();

    writer.push_blocking(1);

    bencher.iter(|| {
        black_box(reader.pop_ref());
    });
}

fn read_empty(bencher: &mut Bencher) {
    let (_writer, reader) = fifo::<i32, 128>();

    bencher.iter(|| {
        black_box(reader.pop_ref());
    });
}

fn read_multi(bencher: &mut Bencher) {
    let (writer, reader) = fifo::<_, 128>();

    for value in 0..128 {
        writer.push_blocking(value);
    }

    bencher.iter(|| {
        while let Some(value) = reader.pop_ref() {
            black_box(value);
        }
    });
}

fn fifo_benchmark(c: &mut Criterion) {
    c.bench_function("write", write);
    c.bench_function("read", read);
    c.bench_function("read_empty", read_empty);
    c.bench_function("read_multi", read_multi);
}

criterion_group!(benches, fifo_benchmark);
criterion_main!(benches);
