#![cfg(loom)]

use {
    loom::{
        sync::{Arc, Mutex},
        thread,
    },
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
fn reading_on_real_time_thread_with_multiple_simultaneously_writers() {
    loom::model(|| {
        let (writer, mut reader) = realtime_reader(Big::default());
        let writer = Arc::new(Mutex::new(writer));

        const WRITERS: i64 = 2;
        const WRITES: i64 = 3;

        for _ in 0..WRITERS {
            thread::spawn({
                let writer = Arc::clone(&writer);
                move || {
                    for _ in 0..WRITES {
                        writer.lock().unwrap().update(|mut value| {
                            value.count += 1;
                            value
                        });
                    }
                }
            });
        }

        let value = reader.get();
        assert!(value.count >= 0 && value.count <= WRITERS * WRITES)
    });
}

#[test]
fn writing_on_real_time_thread_with_multiple_simultaneously_readers() {
    loom::model(|| {
        let (reader, mut writer) = realtime_writer(Big::default());
        let reader = Arc::new(Mutex::new(reader));

        const READERS: i64 = 2;
        const READS: i64 = 2;

        writer.set(Big::new(1));

        for _ in 0..READERS {
            thread::spawn({
                let reader = Arc::clone(&reader);
                move || {
                    let mut last_read = None;

                    for _ in 0..READS {
                        let value = reader.lock().unwrap().get_ref().count;

                        assert!(value == 1 || value == 2);
                        assert!(last_read.is_none() || last_read <= Some(value));

                        last_read = Some(value);
                    }
                }
            });
        }

        writer.set(Big::new(2));
    });
}
