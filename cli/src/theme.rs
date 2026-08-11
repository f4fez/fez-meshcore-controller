// Copyright 2026 Florian MAZEN (F4FEZ)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Color palette and small display helpers shared by the wizard, the
//! `status` command and the TUI.

use console::{style, Style};

pub fn primary() -> Style {
    Style::new().cyan().bold()
}

pub fn accent() -> Style {
    Style::new().magenta().bold()
}

pub fn success() -> Style {
    Style::new().green().bold()
}

pub fn danger() -> Style {
    Style::new().red().bold()
}

pub fn muted() -> Style {
    Style::new().dim()
}

pub fn warning() -> Style {
    Style::new().yellow().bold()
}

/// Banner printed when the CLI starts.
pub fn print_banner() {
    println!();
    println!(
        "  {} {}",
        style("📡").bold(),
        primary().apply_to("fez-mesh-controller")
    );
    println!(
        "  {}",
        muted().apply_to("Controller & monitor for a MeshCore network 🕸️")
    );
    println!();
}

pub fn section(title: &str, emoji: &str) {
    println!();
    println!("{} {}", emoji, primary().apply_to(title));
}

pub fn success_line(text: &str) {
    println!("  {} {}", style("✅").green(), success().apply_to(text));
}

pub fn error_line(text: &str) {
    println!("  {} {}", style("❌").red(), danger().apply_to(text));
}

pub fn info_line(text: &str) {
    println!("  {} {text}", style("ℹ️").cyan());
}
