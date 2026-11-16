use super::*;

impl SaveGameManagerApp {
    pub(super) fn render_logs_tab(&mut self, ui: &mut egui::Ui, pal: &JizihubPalette) {
        let mut copy_filtered = false;
        let mut clear_logs = false;

        glass_frame_1(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.strong("Activity & Sync Logs");
                render_badge(
                    ui,
                    pal,
                    &format!("{} entries", self.logs.len()),
                    false,
                );
                ui.separator();
                ui.label("Level:");
                for (lvl_opt, label) in [
                    (None, "All"),
                    (Some(LogLevel::Info), "INFO"),
                    (Some(LogLevel::Success), "OK"),
                    (Some(LogLevel::Warning), "WARN"),
                    (Some(LogLevel::Error), "ERROR"),
                ] {
                    ui.selectable_value(&mut self.log_filter_level, lvl_opt, label);
                }
                ui.separator();
                ui.label("Filter:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.log_search)
                        .hint_text("search logs...")
                        .desired_width((ui.available_width() - 250.0).max(160.0)),
                );
                if outline_orange_button(ui, pal, true, "Copy Filtered Logs").clicked() {
                    copy_filtered = true;
                }
                if ui.button("Clear Logs").clicked() {
                    clear_logs = true;
                }
            });
            ui.small(
                egui::RichText::new(format!("Persisted log file: {}", self.log_file_path.display()))
                    .monospace()
                    .color(pal.text_3),
            );
        });

        ui.add_space(8.0);

        let search_lower = self.log_search.trim().to_lowercase();
        let filtered: Vec<LogEntry> = self
            .logs
            .iter()
            .filter(|entry| {
                if let Some(lvl) = self.log_filter_level
                    && entry.level != lvl
                {
                    return false;
                }
                if search_lower.is_empty() {
                    true
                } else {
                    entry.message.to_lowercase().contains(&search_lower)
                        || entry.source.to_lowercase().contains(&search_lower)
                        || entry.level.label().to_lowercase().contains(&search_lower)
                }
            })
            .cloned()
            .collect();

        if copy_filtered {
            let joined = filtered
                .iter()
                .map(|e| format!("[{}] [{}] [{}] {}", e.timestamp, e.level.label(), e.source, e.message))
                .collect::<Vec<_>>()
                .join("\n");
            ui.ctx().copy_text(joined);
            self.set_status(format!("Copied {} filtered log entry(ies) to clipboard.", filtered.len()), false);
        }
        if clear_logs {
            self.logs.clear();
            self.push_log(LogLevel::Info, "logs", "Cleared in-app log view.");
            return;
        }

        glass_frame_1(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let avail_h = ui.available_height().max(260.0);
            ui.set_min_height(avail_h);
            egui::ScrollArea::vertical()
                .id_salt("logs_table_scroll")
                .auto_shrink([false, false])
                .max_height(avail_h)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    if filtered.is_empty() {
                        ui.small("No log entries match the current filter.");
                        return;
                    }
                    for entry in &filtered {
                        let lvl_color = match entry.level {
                            LogLevel::Error => pal.error_fg,
                            LogLevel::Warning => pal.warn_fg,
                            LogLevel::Success => pal.ok_fg,
                            LogLevel::Info => pal.text_2,
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new(&entry.timestamp)
                                    .monospace()
                                    .small()
                                    .color(pal.text_3),
                            );
                            ui.label(
                                egui::RichText::new(format!("[{}]", entry.level.label()))
                                    .monospace()
                                    .small()
                                    .strong()
                                    .color(lvl_color),
                            );
                            ui.label(
                                egui::RichText::new(format!("[{}]", entry.source))
                                    .monospace()
                                    .small()
                                    .color(pal.text_3),
                            );
                            ui.label(
                                egui::RichText::new(&entry.message)
                                    .monospace()
                                    .small()
                                    .color(pal.text_1),
                            );
                        });
                    }
                });
        });
    }
}
