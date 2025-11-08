use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

pub fn progress_bar(total: u64, label: &str) -> ProgressBar {
    if total == 0 {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::with_template(
                "{prefix:.bold} {spinner} {elapsed_precise} (eta {eta}) {msg}",
            )
            .expect("valid spinner template"),
        );
        pb.set_prefix(label.to_string());
        pb.enable_steady_tick(Duration::from_millis(100));
        pb
    } else {
        let pb = ProgressBar::new(total);
        pb.set_style(
            ProgressStyle::with_template(
                "{prefix:.bold} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} eta {eta_precise} {msg}",
            )
            .expect("valid bar template")
            .progress_chars("##-"),
        );
        pb.set_prefix(label.to_string());
        pb
    }
}
