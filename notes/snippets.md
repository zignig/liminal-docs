```
use tokio_stream::{self as stream, StreamExt};
use tokio::time::{self, Duration};

#[tokio::main]
async fn main() {
    let mut my_stream = stream::iter(vec![1, 2, 3, 4, 5]);
    let mut timer = time::interval(Duration::from_millis(500));

    loop {
        tokio::select! {
            // Branch for the stream
            Some(item) = my_stream.next() => {
                println!("Received item from stream: {}", item);
            },
            // Branch for the timer
            _ = timer.tick() => {
                println!("Timer ticked!");
            },
            // Optional: A default branch or a branch for a shutdown signal
            else => {
                // This branch executes if all other branches are disabled or complete
                println!("No more stream items or timer ticks, exiting.");
                break;
            }
        }
    }
}

```