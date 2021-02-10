use super::gst::pipeline_runner::Pipeline;
use super::stream_backend::StreamBackend;

use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Debug)]
struct VideoStreamUdpState {
    run: bool,
    kill: bool,
    pipeline: Pipeline,
}

pub struct VideoStreamUdp {
    state: Arc<Mutex<VideoStreamUdpState>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Default for VideoStreamUdpState {
    fn default() -> Self {
        Self {
            run: true,
            kill: false,
            pipeline: Default::default(),
        }
    }
}

impl Default for VideoStreamUdp {
    fn default() -> Self {
        let state: Arc<Mutex<VideoStreamUdpState>> = Default::default();
        let thread_state = state.clone();
        Self {
            state,
            thread: Some(thread::spawn(move || {
                while thread_state.lock().unwrap().run {
                    println!("state: {:#?}", thread_state);
                    std::thread::sleep(std::time::Duration::from_millis(2000));
                }
            })),
        }
    }
}

impl Drop for VideoStreamUdp {
    fn drop(&mut self) {
        // Kill the thread and wait for it
        self.state.lock().unwrap().kill = true;
        self.thread.take().unwrap().join();
    }
}

impl StreamBackend for VideoStreamUdp {
    fn start(&mut self) -> bool {
        self.state.lock().unwrap().run = true;
        return true;
    }

    fn stop(&mut self) -> bool {
        self.state.lock().unwrap().run = false;
        return true;
    }

    fn restart(&mut self) {
        unimplemented!();
    }
}
