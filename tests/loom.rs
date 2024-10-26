#![cfg(loom)]

use {
    loom::thread,
    real_time::{reader::realtime_reader, writer::realtime_writer},
};

#[derive(Copy, Clone)]
struct Big {
    count: i64,
    _padding: [u8; 64],
}

impl Big {
    fn new(count: i64) -> Self {
        Self {
            count,
            _padding: [0; 64],
        }
    }
}

impl Default for Big {
    fn default() -> Self {
        Self::new(0)
    }
}

#[test]
fn reading_on_real_time_thread() {
    loom::model(|| {
        let (writer, reader) = realtime_reader(Big::default());

        const READS: usize = 3;
        const WRITES: usize = 3;

        thread::spawn({
            move || {
                for value in (0..WRITES).map(|value| Big::new(value as i64)) {
                    writer.set(value);
                }
            }
        });

        let reads = (0..READS).map(|_| reader.get().count).collect::<Vec<_>>();

        assert!(reads.len() == READS);
        assert!(reads.iter().all(|&value| value >= 0));
        assert!(reads.iter().all(|&value| value <= WRITES as i64));
        assert!(reads.windows(2).all(|window| window[0] <= window[1]));
    });
}

#[test]
fn writing_on_real_time_thread() {
    loom::model(|| {
        let (reader, writer) = realtime_writer(Big::default());

        const READS: usize = 3;
        const WRITES: usize = 3;

        thread::spawn({
            move || {
                for value in (0..WRITES).map(|value| Big::new(value as i64)) {
                    writer.set(value);
                }
            }
        });

        let reads = (0..READS).map(|_| reader.get().count).collect::<Vec<_>>();

        assert!(reads.len() == READS);
        assert!(reads.iter().all(|&value| value >= 0));
        assert!(reads.iter().all(|&value| value <= WRITES as i64));
        assert!(reads.windows(2).all(|window| window[0] <= window[1]));
    });
}

#[test]
fn single_consumer_single_producer_fifo() {
    loom::model(|| {
        let (tx, rx) = real_time::fifo::fifo(1);

        thread::spawn(move || {
            assert!(tx.push(1).is_ok());
            let _ = tx.push(2);
        });

        assert!(matches!(rx.pop(), None | Some(1)));
        assert!(matches!(rx.pop(), None | Some(2)));
        assert!(rx.pop().is_none());
    })
}
