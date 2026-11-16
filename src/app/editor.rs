use super::*;

impl SaveGameManagerApp {
    pub(super) fn render_save_editor_tab(&mut self, ui: &mut egui::Ui, pal: &JizihubPalette) {
        let mut open_path: Option<PathBuf> = None;

        glass_frame_1(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.strong("Save File Viewer & Editor");
                ui.small(
                    egui::RichText::new(
                        "Inspect save files across your synced games. Text saves are editable with optional .sgm.bak backups; binary saves open in read-only hex & strings view.",
                    )
                    .color(pal.text_3),
                );
            });
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.set_width(ui.available_width());
                ui.label("Game:");
                let current_game_label = self
                    .selected_game_id
                    .as_deref()
                    .and_then(|id| self.config.games.iter().find(|g| g.id == id))
                    .map(|g| format!("[{}] {}", g.provider.display_name(), g.name))
                    .unwrap_or_else(|| "Select a game...".to_string());

                let mut game_changed = false;
                egui::ComboBox::from_id_salt("editor_game_selector")
                    .width(280.0)
                    .selected_text(current_game_label)
                    .show_ui(ui, |ui| {
                        for game in &self.config.games {
                            let label = format!("[{}] {}", game.provider.display_name(), game.name);
                            if ui
                                .selectable_value(
                                    &mut self.selected_game_id,
                                    Some(game.id.clone()),
                                    label,
                                )
                                .changed()
                            {
                                game_changed = true;
                            }
                        }
                    });
                if game_changed {
                    self.refresh_selected_game_files();
                }

                let selected_game_dir = self
                    .selected_game_id
                    .as_deref()
                    .and_then(|id| self.config.games.iter().find(|g| g.id == id))
                    .map(|g| g.save_dir.clone());

                if let Some(save_dir) = &selected_game_dir {
                    ui.label("Save File:");
                    let loaded_rel = self
                        .save_editor
                        .loaded
                        .as_ref()
                        .and_then(|f| f.path.strip_prefix(save_dir).ok())
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "Choose file from folder...".to_string());
                    egui::ComboBox::from_id_salt("editor_file_selector")
                        .width((ui.available_width() - 260.0).max(200.0))
                        .selected_text(loaded_rel)
                        .show_ui(ui, |ui| {
                            if self.available_files_cache.is_empty() {
                                ui.small("No files found in save folder.");
                            }
                            for rel in &self.available_files_cache {
                                let candidate = save_dir.join(rel);
                                let is_cur = self
                                    .save_editor
                                    .loaded
                                    .as_ref()
                                    .is_some_and(|f| f.path == candidate);
                                if ui.selectable_label(is_cur, rel).clicked() {
                                    open_path = Some(candidate);
                                }
                            }
                        });
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if outline_orange_button(ui, pal, true, "Browse File...").clicked() {
                        let mut dialog = rfd::FileDialog::new();
                        if let Some(dir) = selected_game_dir.as_ref().filter(|d| d.exists()) {
                            dialog = dialog.set_directory(dir);
                        }
                        if let Some(file) = dialog.pick_file() {
                            open_path = Some(file);
                        }
                    }
                    if selected_game_dir.is_some() && ui.button("Refresh Files").clicked() {
                        self.refresh_selected_game_files();
                    }
                });
            });
        });

        ui.add_space(8.0);

        let selected_game_dir = self
            .selected_game_id
            .as_deref()
            .and_then(|id| self.config.games.iter().find(|g| g.id == id))
            .map(|g| g.save_dir.clone());

        let avail = ui.available_size();
        let list_w = (avail.x * 0.26).clamp(230.0, 340.0);
        let gap = ui.spacing().item_spacing.x;
        let detail_w = (avail.x - list_w - gap).max(360.0);
        let panel_h = avail.y.max(300.0);

        let mut action_save = false;
        let mut action_reload = false;
        let mut action_format_json = false;
        let mut action_validate = false;

        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(list_w, panel_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    glass_frame_1(pal).show(ui, |ui| {
                        ui.set_width(list_w - 34.0);
                        ui.set_min_height(panel_h - 34.0);
                        ui.horizontal(|ui| {
                            ui.strong("Files in Save Folder");
                            render_badge(
                                ui,
                                pal,
                                &format!("{}", self.available_files_cache.len()),
                                false,
                            );
                        });
                        ui.separator();

                        let list_h = ui.available_height().max(120.0);
                        egui::ScrollArea::vertical()
                            .id_salt("editor_files_list_scroll")
                            .auto_shrink([false, false])
                            .max_height(list_h)
                            .show(ui, |ui| {
                                let Some(save_dir) = &selected_game_dir else {
                                    ui.label(
                                        egui::RichText::new("Select a game above to browse its save files.")
                                            .color(pal.text_3),
                                    );
                                    return;
                                };
                                if self.available_files_cache.is_empty() {
                                    ui.label(
                                        egui::RichText::new("No files found in save folder.")
                                            .color(pal.text_3),
                                    );
                                    return;
                                }
                                ui.spacing_mut().item_spacing.y = 3.0;
                                for rel in &self.available_files_cache {
                                    let candidate = save_dir.join(rel);
                                    let is_cur = self
                                        .save_editor
                                        .loaded
                                        .as_ref()
                                        .is_some_and(|f| f.path == candidate);
                                    let frame = egui::Frame::new()
                                        .fill(if is_cur {
                                            pal.accent_soft
                                        } else {
                                            egui::Color32::TRANSPARENT
                                        })
                                        .stroke(egui::Stroke::new(
                                            1.0,
                                            if is_cur {
                                                pal.accent_border
                                            } else {
                                                egui::Color32::TRANSPARENT
                                            },
                                        ))
                                        .corner_radius(egui::CornerRadius::same(4))
                                        .inner_margin(egui::Margin::symmetric(8, 4));
                                    let resp = frame
                                        .show(ui, |ui| {
                                            ui.set_width(ui.available_width());
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(rel)
                                                        .monospace()
                                                        .small()
                                                        .color(if is_cur {
                                                            pal.accent_text
                                                        } else {
                                                            pal.text_1
                                                        }),
                                                )
                                                .truncate()
                                                .selectable(false),
                                            );
                                        })
                                        .response
                                        .interact(egui::Sense::click())
                                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                                        .on_hover_text(rel);
                                    if resp.clicked() {
                                        open_path = Some(candidate);
                                    }
                                }
                            });
                    });
                },
            );

            if let Some(path) = open_path.take() {
                self.open_file_in_editor(&path);
            }

            ui.allocate_ui_with_layout(
                egui::vec2(detail_w, panel_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    glass_frame_1(pal).show(ui, |ui| {
                        ui.set_width(detail_w - 34.0);
                        ui.set_min_height(panel_h - 34.0);

                        let Some(loaded) = self.save_editor.loaded.clone() else {
                            ui.centered_and_justified(|ui| {
                                ui.label(
                                    egui::RichText::new(
                                        "No save file loaded.\nPick a file on the left, select one above, click \"Browse File...\", or click \"View\" next to any file in Games.",
                                    )
                                    .color(pal.text_3),
                                );
                            });
                            return;
                        };

                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new(loaded.path.display().to_string())
                                    .monospace()
                                    .strong()
                                    .color(pal.text_1),
                            );
                            render_badge(
                                ui,
                                pal,
                                loaded.format.display_name(),
                                loaded.editable,
                            );
                            render_badge(
                                ui,
                                pal,
                                &format!("{} bytes", loaded.size_bytes),
                                false,
                            );
                            if self.save_editor.dirty {
                                render_badge(ui, pal, "Unsaved changes", true);
                            }
                        });

                        ui.add_space(6.0);
                        if loaded.editable {
                            ui.horizontal_wrapped(|ui| {
                                if accent_button(ui, pal, true, "Save Changes").clicked() {
                                    action_save = true;
                                }
                                if loaded.format == SaveFileFormat::Json {
                                    if ui.button("Format JSON").clicked() {
                                        action_format_json = true;
                                    }
                                    if ui.button("Validate JSON").clicked() {
                                        action_validate = true;
                                    }
                                } else if ui.button("Validate").clicked() {
                                    action_validate = true;
                                }
                                if ui.button("Reload from Disk").clicked() {
                                    action_reload = true;
                                }
                                ui.checkbox(
                                    &mut self.save_editor.create_backup_on_save,
                                    "Create .sgm.bak backup before saving",
                                );
                            });
                        } else {
                            ui.horizontal_wrapped(|ui| {
                                if ui.button("Reload from Disk").clicked() {
                                    action_reload = true;
                                }
                                ui.selectable_value(
                                    &mut self.save_editor.binary_view,
                                    BinaryViewMode::HexDump,
                                    "Hex + ASCII",
                                );
                                ui.selectable_value(
                                    &mut self.save_editor.binary_view,
                                    BinaryViewMode::Strings,
                                    format!("Extracted Strings ({})", loaded.extracted_strings.len()),
                                );
                                ui.small(
                                    egui::RichText::new(
                                        "Binary save format is read-only to prevent corrupting game checksums or binary layouts.",
                                    )
                                    .color(pal.text_3),
                                );
                            });
                        }

                        if let Some((msg, is_err)) = &self.save_editor.last_message {
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(msg)
                                    .small()
                                    .color(if *is_err { pal.error_fg } else { pal.ok_fg }),
                            );
                        }

                        ui.add_space(6.0);
                        let editor_h = ui.available_height().max(200.0);
                        let desired_rows = ((editor_h / 18.0).ceil() as usize).max(24);
                        if loaded.editable {
                            egui::ScrollArea::vertical()
                                .id_salt("save_editor_text_scroll")
                                .auto_shrink([false, false])
                                .max_height(editor_h)
                                .show(ui, |ui| {
                                    let resp = ui.add(
                                        egui::TextEdit::multiline(&mut self.save_editor.edit_buffer)
                                            .code_editor()
                                            .desired_width(f32::INFINITY)
                                            .desired_rows(desired_rows),
                                    );
                                    if resp.changed() {
                                        self.save_editor.dirty = true;
                                    }
                                });
                        } else {
                            match self.save_editor.binary_view {
                                BinaryViewMode::HexDump => {
                                    let mut hex_copy = loaded.hex_preview.clone();
                                    egui::ScrollArea::vertical()
                                        .id_salt("save_editor_hex_scroll")
                                        .auto_shrink([false, false])
                                        .max_height(editor_h)
                                        .show(ui, |ui| {
                                            ui.add(
                                                egui::TextEdit::multiline(&mut hex_copy)
                                                    .code_editor()
                                                    .desired_width(f32::INFINITY)
                                                    .desired_rows(desired_rows),
                                            );
                                        });
                                }
                                BinaryViewMode::Strings => {
                                    ui.horizontal(|ui| {
                                        ui.label("Filter strings:");
                                        ui.add(
                                            egui::TextEdit::singleline(
                                                &mut self.save_editor.string_filter,
                                            )
                                            .desired_width(ui.available_width()),
                                        );
                                    });
                                    let needle =
                                        self.save_editor.string_filter.trim().to_lowercase();
                                    egui::ScrollArea::vertical()
                                        .id_salt("save_editor_strings_scroll")
                                        .auto_shrink([false, false])
                                        .max_height(ui.available_height().max(160.0))
                                        .show(ui, |ui| {
                                            let mut shown = 0usize;
                                            for line in &loaded.extracted_strings {
                                                if !needle.is_empty()
                                                    && !line.to_lowercase().contains(&needle)
                                                {
                                                    continue;
                                                }
                                                shown += 1;
                                                ui.label(
                                                    egui::RichText::new(line)
                                                        .monospace()
                                                        .small()
                                                        .color(pal.text_2),
                                                );
                                            }
                                            if shown == 0 {
                                                ui.small("No matching printable strings found.");
                                            }
                                        });
                                }
                            }
                        }
                    });
                },
            );
        });

        let Some(loaded) = self.save_editor.loaded.clone() else {
            return;
        };

        if action_format_json {
            match format_json_content(&self.save_editor.edit_buffer) {
                Ok(pretty) => {
                    self.save_editor.edit_buffer = pretty;
                    self.save_editor.dirty = true;
                    let msg = "Formatted JSON content.".to_string();
                    self.save_editor.last_message = Some((msg.clone(), false));
                    self.set_status(msg, false);
                }
                Err(err) => {
                    let msg = format!("JSON format error: {err}");
                    self.save_editor.last_message = Some((msg.clone(), true));
                    self.set_status(msg, true);
                }
            }
        } else if action_validate {
            match validate_save_content(loaded.format, &self.save_editor.edit_buffer) {
                Ok(()) => {
                    let msg = "Validation passed cleanly.".to_string();
                    self.save_editor.last_message = Some((msg.clone(), false));
                    self.set_status(msg, false);
                }
                Err(err) => {
                    let msg = format!("Validation failed: {err}");
                    self.save_editor.last_message = Some((msg.clone(), true));
                    self.set_status(msg, true);
                }
            }
        } else if action_save {
            match save_edited_file(
                &loaded.path,
                loaded.format,
                &self.save_editor.edit_buffer,
                self.save_editor.create_backup_on_save,
            ) {
                Ok(bak_opt) => {
                    self.save_editor.dirty = false;
                    let bak_note = bak_opt
                        .map(|p| format!(" (backup: {})", p.display()))
                        .unwrap_or_default();
                    let msg = format!("Saved changes to {}{}", loaded.path.display(), bak_note);
                    self.save_editor.last_message = Some((msg.clone(), false));
                    self.set_status(msg, false);
                    self.open_file_in_editor(&loaded.path);
                    self.refresh_selected_game_files();
                }
                Err(err) => {
                    let msg = format!("Failed to save file: {err}");
                    self.save_editor.last_message = Some((msg.clone(), true));
                    self.set_status(msg, true);
                }
            }
        } else if action_reload {
            self.open_file_in_editor(&loaded.path);
        }
    }
}
