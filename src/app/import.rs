use super::*;

impl SaveGameManagerApp {
    pub(super) fn render_import_tab(&mut self, ui: &mut egui::Ui, pal: &JizihubPalette) {
        let avail = ui.available_size();
        if avail.x >= 920.0 {
            let gap = ui.spacing().item_spacing.x;
            let left_w = (avail.x * 0.48).max(420.0);
            let right_w = (avail.x - left_w - gap).max(420.0);
            let panel_h = avail.y.max(420.0);

            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(left_w, panel_h),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("import_left_scroll")
                            .auto_shrink([false, false])
                            .max_height(panel_h)
                            .show(ui, |ui| {
                                let start_y = ui.cursor().top();
                                self.render_import_reinstate_card(ui, pal);
                                ui.add_space(8.0);
                                let used_h = (ui.cursor().top() - start_y).max(0.0);
                                let online_min_inner_h = (panel_h - used_h - 34.0).max(220.0);
                                self.render_import_online_save_card(ui, pal, Some(online_min_inner_h));
                            });
                    },
                );
                ui.allocate_ui_with_layout(
                    egui::vec2(right_w, panel_h),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        let shared_min_inner_h = (panel_h - 34.0).max(360.0);
                        self.render_import_shared_repo_card(ui, pal, shared_min_inner_h);
                    },
                );
            });
        } else {
            egui::ScrollArea::vertical()
                .id_salt("import_reinstate_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.render_import_reinstate_card(ui, pal);
                    ui.add_space(10.0);
                    self.render_import_shared_repo_card(ui, pal, 360.0);
                    ui.add_space(10.0);
                    self.render_import_online_save_card(ui, pal, None);
                });
        }
    }

    fn render_import_reinstate_card(&mut self, ui: &mut egui::Ui, pal: &JizihubPalette) {
        glass_frame_1(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                render_badge(ui, pal, "SYSTEM RESET RECOVERY", true);
                ui.heading("Re-instate Full Structure from Save Repository");
            });
            ui.label(
                egui::RichText::new(
                    "Restores all games, save state labels, and save folders onto a fresh or reset system using sgm_repo_metadata.json and provider folder structure.",
                )
                .color(pal.text_2),
            );
            ui.add_space(6.0);
            ui.small(
                egui::RichText::new("Source Repo (GitHub URL or local folder, leave blank to use current):")
                    .color(pal.text_3),
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.import_state.reinstate_source)
                        .desired_width((ui.available_width() - 165.0).max(160.0)),
                );
                if ui.button("Browse Local Repo...").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    self.import_state.reinstate_source = dir.display().to_string();
                }
            });
            ui.small(
                egui::RichText::new("Fallback local saves folder (when a game isn't installed yet):")
                    .color(pal.text_3),
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.import_state.reinstate_fallback_dir)
                        .desired_width((ui.available_width() - 95.0).max(160.0)),
                );
                if ui.button("Browse...").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    self.import_state.reinstate_fallback_dir = dir.display().to_string();
                }
            });
            ui.add_space(4.0);
            if accent_button(
                ui,
                pal,
                !self.busy,
                "Re-instate All Games & Save States Now",
            )
            .clicked()
            {
                let src = if self.import_state.reinstate_source.trim().is_empty() {
                    None
                } else {
                    Some(self.import_state.reinstate_source.clone())
                };
                let fallback = PathBuf::from(self.import_state.reinstate_fallback_dir.trim());
                self.start_reinstate(src, fallback);
            }
        });
    }

    fn render_import_shared_repo_card(
        &mut self,
        ui: &mut egui::Ui,
        pal: &JizihubPalette,
        min_inner_h: f32,
    ) {
        glass_frame_1(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(min_inner_h);
            let card_start_y = ui.cursor().top();
            ui.horizontal_wrapped(|ui| {
                render_badge(ui, pal, "SHARED REPO", false);
                ui.heading("Import Game States from Another Accessible Repository");
            });
            ui.label(
                egui::RichText::new(
                    "Browse another user's repository (Git URL or cloned folder), pick a game and specific labeled commit/save state, and import it into your setup.",
                )
                .color(pal.text_2),
            );
            ui.add_space(6.0);
            ui.small(egui::RichText::new("External Repo URL or Path:").color(pal.text_3));
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.import_state.ext_repo_source)
                        .hint_text("https://github.com/user/saves.git or local folder")
                        .desired_width((ui.available_width() - 315.0).max(140.0)),
                );
                if ui.button("Browse Folder...").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    self.import_state.ext_repo_source = dir.display().to_string();
                }
                if outline_orange_button(
                    ui,
                    pal,
                    !self.busy && !self.import_state.ext_repo_source.trim().is_empty(),
                    "Inspect Shared Repository",
                )
                .clicked()
                {
                    self.start_inspect_external(self.import_state.ext_repo_source.clone());
                }
            });

            ui.add_space(6.0);
            let used_above = (ui.cursor().top() - card_start_y).max(0.0);
            let body_min_h = (min_inner_h - used_above - 26.0).max(180.0);
            glass_frame_2(pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(body_min_h);
                let body_start_y = ui.cursor().top();

                let Some(inspection) = self.import_state.ext_inspection.clone() else {
                    ui.vertical(|ui| {
                        ui.strong("Inspect a shared save repository");
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(
                                "Enter a Git URL or pick a local clone above and click \"Inspect Shared Repository\".",
                            )
                            .color(pal.text_2),
                        );
                        ui.add_space(6.0);
                        for tip in [
                            "• Discovered games and providers from the repository will appear here.",
                            "• Browse the full commit timeline, save-state labels, and changed file counts for any game.",
                            "• Import an individual labeled snapshot or bring in all games at their latest state.",
                        ] {
                            ui.small(egui::RichText::new(tip).color(pal.text_3));
                        }
                    });
                    return;
                };

                if inspection.games.is_empty() {
                    ui.label("No game save folders detected in this repository.");
                    return;
                }

                ui.horizontal(|ui| {
                    ui.label("Discovered Game:");
                    let selected_summary = self
                        .import_state
                        .ext_selected_game_id
                        .as_deref()
                        .and_then(|id| inspection.games.iter().find(|g| g.id == id))
                        .map(format_repo_entry_summary)
                        .unwrap_or_else(|| "Select a game...".to_string());

                    egui::ComboBox::from_id_salt("ext_repo_game_select")
                        .width((ui.available_width() - 125.0).max(180.0))
                        .selected_text(selected_summary)
                        .show_ui(ui, |ui| {
                            for g in &inspection.games {
                                let txt = format_repo_entry_summary(g);
                                if ui
                                    .selectable_value(
                                        &mut self.import_state.ext_selected_game_id,
                                        Some(g.id.clone()),
                                        txt,
                                    )
                                    .changed()
                                {
                                    self.import_state.ext_selected_commit = None;
                                    self.import_state.ext_target_provider = g.provider;
                                    self.import_state.ext_target_save_dir = PathBuf::from(
                                        self.import_state.reinstate_fallback_dir.trim(),
                                    )
                                    .join(g.provider.slug())
                                    .join(&g.game_slug)
                                    .display()
                                    .to_string();
                                }
                            }
                        });
                });

                let Some(sel_id) = self.import_state.ext_selected_game_id.clone() else {
                    return;
                };
                let Some(src_game) = inspection.games.iter().find(|g| g.id == sel_id).cloned() else {
                    return;
                };

                let commits = self
                    .import_state
                    .ext_game_commits
                    .get(&sel_id)
                    .cloned()
                    .unwrap_or_default();

                ui.horizontal(|ui| {
                    ui.label("Save State / Commit:");
                    let current_commit_label = match &self.import_state.ext_selected_commit {
                        None => "Latest (HEAD)".to_string(),
                        Some(hash) => commits
                            .iter()
                            .find(|c| &c.commit_hash == hash)
                            .map(|c| {
                                let lbl = c
                                    .label
                                    .as_deref()
                                    .map(|l| format!(" [{l}]"))
                                    .unwrap_or_default();
                                format!("{} — {}{}", c.short_hash, c.message, lbl)
                            })
                            .unwrap_or_else(|| hash.clone()),
                    };
                    egui::ComboBox::from_id_salt("ext_repo_commit_select")
                        .width((ui.available_width() - 145.0).max(180.0))
                        .selected_text(current_commit_label)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.import_state.ext_selected_commit,
                                None,
                                "Latest (HEAD)",
                            );
                            for c in &commits {
                                let lbl = c
                                    .label
                                    .as_deref()
                                    .map(|l| format!(" [{l}]"))
                                    .unwrap_or_default();
                                let text = format!("{} — {}{}", c.short_hash, c.message, lbl);
                                ui.selectable_value(
                                    &mut self.import_state.ext_selected_commit,
                                    Some(c.commit_hash.clone()),
                                    text,
                                );
                            }
                        });
                });

                ui.horizontal(|ui| {
                    ui.label("Target Provider:");
                    egui::ComboBox::from_id_salt("ext_target_provider")
                        .width(140.0)
                        .selected_text(self.import_state.ext_target_provider.display_name())
                        .show_ui(ui, |ui| {
                            for &p in Provider::all() {
                                ui.selectable_value(
                                    &mut self.import_state.ext_target_provider,
                                    p,
                                    p.display_name(),
                                );
                            }
                        });
                    ui.label("Label:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.import_state.ext_custom_label)
                            .hint_text("Keep original or override...")
                            .desired_width(ui.available_width().max(120.0)),
                    );
                });

                ui.horizontal(|ui| {
                    ui.label("Target Local Save Folder:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.import_state.ext_target_save_dir)
                            .desired_width((ui.available_width() - 95.0).max(140.0)),
                    );
                    if ui.button("Browse...").clicked()
                        && let Some(dir) = rfd::FileDialog::new().pick_folder()
                    {
                        self.import_state.ext_target_save_dir = dir.display().to_string();
                    }
                });

                ui.horizontal_wrapped(|ui| {
                    if accent_button(ui, pal, !self.busy, "Import Selected Game State").clicked() {
                        let req = ExternalImportRequest {
                            source_game: src_game.clone(),
                            commit_hash: self.import_state.ext_selected_commit.clone(),
                            target_provider: self.import_state.ext_target_provider,
                            target_save_dir: PathBuf::from(
                                self.import_state.ext_target_save_dir.trim(),
                            ),
                            custom_label: if self.import_state.ext_custom_label.trim().is_empty() {
                                None
                            } else {
                                Some(self.import_state.ext_custom_label.trim().to_string())
                            },
                        };
                        self.start_external_import(inspection.repo_path.clone(), req);
                    }

                    if outline_orange_button(ui, pal, !self.busy, "Import All Games (Latest)")
                        .clicked()
                    {
                        self.start_external_import_all(
                            inspection.repo_path.clone(),
                            inspection.clone(),
                        );
                    }
                });

                ui.add_space(4.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.strong(format!("Commit History ({})", commits.len()));
                    if self.import_state.ext_selected_commit.is_some()
                        && ui.small_button("Use Latest (HEAD)").clicked()
                    {
                        self.import_state.ext_selected_commit = None;
                    }
                });

                let commits_used = (ui.cursor().top() - body_start_y).max(0.0);
                let list_h = (body_min_h - commits_used).max(120.0);
                egui::ScrollArea::vertical()
                    .id_salt("ext_repo_commits_list")
                    .auto_shrink([false, false])
                    .max_height(list_h)
                    .show(ui, |ui| {
                        if commits.is_empty() {
                            ui.small(
                                egui::RichText::new("No commits found for this game.")
                                    .color(pal.text_3),
                            );
                            return;
                        }
                        ui.spacing_mut().item_spacing.y = 4.0;
                        for c in &commits {
                            let selected = self
                                .import_state
                                .ext_selected_commit
                                .as_deref()
                                == Some(c.commit_hash.as_str());
                            let frame = egui::Frame::new()
                                .fill(if selected {
                                    pal.accent_soft
                                } else {
                                    pal.glass_1
                                })
                                .stroke(egui::Stroke::new(
                                    1.0,
                                    if selected {
                                        pal.accent_border
                                    } else {
                                        pal.border_glass
                                    },
                                ))
                                .corner_radius(egui::CornerRadius::same(4))
                                .inner_margin(egui::Margin::symmetric(8, 5));
                            let resp = frame
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(
                                            egui::RichText::new(&c.short_hash)
                                                .monospace()
                                                .strong()
                                                .color(if selected {
                                                    pal.accent_text
                                                } else {
                                                    pal.text_1
                                                }),
                                        );
                                        ui.label(
                                            egui::RichText::new(&c.timestamp)
                                                .small()
                                                .color(pal.text_3),
                                        );
                                        if let Some(lbl) = &c.label {
                                            render_badge(ui, pal, lbl, true);
                                        }
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.small(
                                                    egui::RichText::new(format!(
                                                        "{} file(s)",
                                                        c.changed_files.len()
                                                    ))
                                                    .color(pal.text_3),
                                                );
                                            },
                                        );
                                    });
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(&c.message)
                                                .small()
                                                .color(pal.text_2),
                                        )
                                        .truncate()
                                        .selectable(false),
                                    );
                                })
                                .response
                                .interact(egui::Sense::click())
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if resp.clicked() {
                                self.import_state.ext_selected_commit = Some(c.commit_hash.clone());
                            }
                        }
                    });
            });
        });
    }

    fn render_import_online_save_card(
        &mut self,
        ui: &mut egui::Ui,
        pal: &JizihubPalette,
        min_inner_h: Option<f32>,
    ) {
        glass_frame_1(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(h) = min_inner_h {
                ui.set_min_height(h);
            }
            ui.horizontal_wrapped(|ui| {
                render_badge(ui, pal, "ONLINE SAVE", true);
                ui.heading("Import Save Game from Online Sources");
            });
            ui.label(
                egui::RichText::new(
                    "Import a downloaded save file, folder, or .zip / .tar.gz archive into an existing or new game. Optionally creates a safety backup commit first.",
                )
                .color(pal.text_2),
            );
            ui.add_space(6.0);

            ui.small(
                egui::RichText::new("Downloaded Save File / Folder / Archive:").color(pal.text_3),
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.import_state.online_source_path)
                        .desired_width((ui.available_width() - 250.0).max(140.0)),
                );
                if ui.button("Pick File / Archive...").clicked()
                    && let Some(file) = rfd::FileDialog::new().pick_file()
                {
                    self.import_state.online_source_path = file.display().to_string();
                }
                if ui.button("Pick Folder...").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    self.import_state.online_source_path = dir.display().to_string();
                }
            });

            ui.horizontal(|ui| {
                ui.label("Target Game:");
                let existing_text = self
                    .import_state
                    .online_target_existing_id
                    .as_deref()
                    .and_then(|id| self.config.games.iter().find(|g| g.id == id))
                    .map(|g| format!("[{}] {}", g.provider.display_name(), g.name))
                    .unwrap_or_else(|| "Create New Game Entry".to_string());

                egui::ComboBox::from_id_salt("online_import_target_game")
                    .width((ui.available_width() - 100.0).max(180.0))
                    .selected_text(existing_text)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_value(
                                &mut self.import_state.online_target_existing_id,
                                None,
                                "Create New Game Entry",
                            )
                            .clicked()
                        {
                            self.import_state.online_game_name.clear();
                            self.import_state.online_target_save_dir.clear();
                        }
                        for g in &self.config.games {
                            let label = format!("[{}] {}", g.provider.display_name(), g.name);
                            if ui
                                .selectable_value(
                                    &mut self.import_state.online_target_existing_id,
                                    Some(g.id.clone()),
                                    label,
                                )
                                .clicked()
                            {
                                self.import_state.online_game_name = g.name.clone();
                                self.import_state.online_provider = g.provider;
                                self.import_state.online_target_save_dir =
                                    g.save_dir.display().to_string();
                            }
                        }
                    });
            });

            ui.horizontal(|ui| {
                ui.label("Game Title:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.import_state.online_game_name)
                        .desired_width((ui.available_width() - 220.0).max(140.0)),
                );
                ui.label("Provider:");
                egui::ComboBox::from_id_salt("online_import_provider")
                    .width(ui.available_width().max(110.0))
                    .selected_text(self.import_state.online_provider.display_name())
                    .show_ui(ui, |ui| {
                        for &p in Provider::all() {
                            ui.selectable_value(
                                &mut self.import_state.online_provider,
                                p,
                                p.display_name(),
                            );
                        }
                    });
            });

            ui.horizontal(|ui| {
                ui.label("Target Local Save Folder:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.import_state.online_target_save_dir)
                        .desired_width((ui.available_width() - 95.0).max(140.0)),
                );
                if ui.button("Browse...").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    self.import_state.online_target_save_dir = dir.display().to_string();
                }
            });

            ui.horizontal_wrapped(|ui| {
                ui.label("Save State Label:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.import_state.online_label)
                        .desired_width((ui.available_width() * 0.4).max(140.0)),
                );
                ui.checkbox(
                    &mut self.import_state.online_backup_existing,
                    "Create 'Pre-import backup' commit of existing saves first",
                );
            });

            ui.add_space(4.0);
            if accent_button(
                ui,
                pal,
                !self.busy
                    && !self.import_state.online_source_path.trim().is_empty()
                    && !self.import_state.online_game_name.trim().is_empty()
                    && !self.import_state.online_target_save_dir.trim().is_empty(),
                "Import Online Save & Commit",
            )
            .clicked()
            {
                let req = OnlineSaveImportRequest {
                    source_path: PathBuf::from(self.import_state.online_source_path.trim()),
                    game_name: self.import_state.online_game_name.trim().to_string(),
                    provider: self.import_state.online_provider,
                    target_save_dir: PathBuf::from(
                        self.import_state.online_target_save_dir.trim(),
                    ),
                    label: if self.import_state.online_label.trim().is_empty() {
                        None
                    } else {
                        Some(self.import_state.online_label.trim().to_string())
                    },
                    backup_existing_before_import: self.import_state.online_backup_existing,
                };
                self.start_online_import(req);
            }
        });
    }
}
