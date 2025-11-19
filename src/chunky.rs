use owo_colors::OwoColorize;

pub fn print_chunky_reminder(min_x: i32, min_z: i32, max_x: i32, max_z: i32) {
    println!();
    println!(
        "{} To finish terrain rendering with Chunky, run these commands in Minecraft:",
        "ℹ".blue().bold()
    );
    println!("  {} chunky shape rectangle", "•".bright_cyan());
    println!(
        "  {} chunky corners {} {} {} {}",
        "•".bright_cyan(),
        min_x,
        min_z,
        max_x,
        max_z
    );
    println!("  {} chunky start", "•".bright_cyan());
    println!();
}
