use super::*;

enum GameAction {
    Sync(Option<String>),
    LaunchRetroArch,
    AttachRetroArchRom,
    UpdateRetroArchMeta {
        meta: Box<crate::models::RetroArchGameMeta>,
        commit: bool,
        ensure_bridge: bool,
        status_note: Option<String>,
    },
    SaveDefaultKbmTemplate(Box<crate::models::KbmProfile>),
    OpenControlsTab,
    OpenFolder(PathBuf),
    OpenEditor(PathBuf),
    ApplySuggestion,
    DismissSuggestion,
    ConfirmLocation,
    ChooseFolder,
    ResolveConflict { use_cloud: bool },
    Restore(CommitInfo),
    BranchProfile(CommitInfo),
    SaveLabel(String, String),
    SwitchProfile(String),
    DeleteProfile(String),
    RenameProfile(String, String),
    CreateProfile,
    Remove,
    Export,
    Undo(PathBuf),
    Rescan,
    RefreshFiles,
}

fn health_color(health: &GameHealth, pal: &JizihubPalette) -> egui::Color32 {
    match health {
        GameHealth::Protected => pal.ok_fg,
        GameHealth::ChangesPending | GameHealth::NeverBackedUp | GameHealth::Running => pal.accent_text,
        GameHealth::Conflict | GameHealth::NeedsReview => pal.warn_fg,
        GameHealth::NoSavesYet | GameHealth::NotInstalled | GameHealth::Unavailable | GameHealth::Paused => pal.text_3,
    }
}

/// Small filled circle in the health colour (the bundled fonts have no bullet glyph).
fn status_dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// Turns an internal commit subject into something a player recognises.
fn humanize_subject(message: &str) -> String {
    let base = message.split(" [Label:").next().unwrap_or(message).trim();
    if let Some(rest) = base.strip_prefix("Sync ") {
        if let Some((count, _)) = rest.split_once(" game save folder(s)") {
            return format!("Snapshot (together with {count} games)");
        }
        return "Snapshot".to_string();
    }
    if let Some(rest) = base.strip_prefix("Rollback ") {
        return format!("Restored an earlier snapshot ({})", rest.rsplit(' ').next().unwrap_or(""));
    }
    if base.starts_with("Combine saves") {
        return "Combined with saves from another PC".to_string();
    }
    if base.starts_with("Switch ") || base.starts_with("Create profile") {
        return base.to_string();
    }
    base.to_string()
}

fn needs_attention(health: &GameHealth) -> bool {
    matches!(health, GameHealth::Conflict | GameHealth::NeedsReview)
}

impl SaveGameManagerApp {
    pub(super) fn render_games_tab(&mut self, ui: &mut egui::Ui, pal: &JizihubPalette) {
        if !self.config.onboarding_done {
            self.render_onboarding(ui, pal);
            ui.add_space(8.0);
        }
        self.render_warnings_banner(ui, pal);
        self.render_scan_banner(ui, pal);
        self.render_untracked_panel(ui, pal);
        if self.add_game_draft.open {
            self.render_add_game_panel(ui, pal);
            ui.add_space(6.0);
        }

        let avail = ui.available_size();
        let list_w = (avail.x * 0.34).clamp(300.0, 520.0);
        let gap = ui.spacing().item_spacing.x;
        let detail_w = (avail.x - list_w - gap).max(320.0);
        let panel_h = avail.y.max(300.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(list_w, panel_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    glass_frame_1(pal).show(ui, |ui| {
                        ui.set_min_height(panel_h - 34.0);
                        ui.set_width(list_w - 34.0);
                        self.render_game_list(ui, pal);
                    });
                },
            );
            ui.allocate_ui_with_layout(
                egui::vec2(detail_w, panel_h),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    glass_frame_1(pal).show(ui, |ui| {
                        ui.set_min_height(panel_h - 34.0);
                        ui.set_width(detail_w - 34.0);
                        self.render_game_detail(ui, pal);
                    });
                },
            );
        });
    }

    fn render_onboarding(&mut self, ui: &mut egui::Ui, pal: &JizihubPalette) {
        glass_frame_3(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                render_badge(ui, pal, "WELCOME", true);
                ui.heading("Let's protect your saves");
            });
            ui.label(
                egui::RichText::new(
                    "SaveVault finds your installed games and where they keep their saves, then keeps a history of every \
                     change on this PC. Nothing leaves this computer unless you connect a cloud later in Settings.",
                )
                .color(pal.text_2),
            );
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                let db_ready = self.db.is_some();
                if accent_button(ui, pal, !self.busy, "Find my games and protect them").clicked() {
                    self.config.onboarding_done = true;
                    self.persist_config();
                    self.start_scan(false);
                }
                if ui.button("I'll add games myself").clicked() {
                    self.config.onboarding_done = true;
                    self.persist_config();
                    self.add_game_draft.open = true;
                }
                ui.small(
                    egui::RichText::new(if db_ready {
                        format!(
                            "Save location database ready ({} games).",
                            self.db.as_ref().map(|d| d.len()).unwrap_or(0)
                        )
                    } else if self.busy {
                        "Preparing the save location database...".to_string()
                    } else {
                        "Save location database not available; SaveVault will use its own scan.".to_string()
                    })
                    .color(pal.text_3),
                );
            });
        });
    }

    fn render_scan_banner(&mut self, ui: &mut egui::Ui, pal: &JizihubPalette) {
        let Some(text) = self.scan_banner.clone() else {
            return;
        };
        let mut dismiss = false;
        let mut apply_all = false;
        glass_frame_2(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                render_badge(ui, pal, "SCAN", true);
                ui.label(text);
                if !self.suggestions.is_empty()
                    && outline_orange_button(ui, pal, !self.busy, "Use all precise locations")
                        .on_hover_text("Switch these games to the exact save folder from the save database. Their history is kept.")
                        .clicked()
                {
                    apply_all = true;
                }
                if !self.suggestions.is_empty() && ui.button("Review one by one").clicked() {
                    self.filter = GameFilter::All;
                    if let Some(id) = self.suggestions.keys().next().cloned() {
                        self.select_game(id);
                        self.detail_tab = DetailTab::Overview;
                    }
                }
                if ui.button("Dismiss").clicked() {
                    dismiss = true;
                }
            });
        });
        ui.add_space(6.0);
        if apply_all {
            let include_config = self.config.include_config_roots;
            let ids: Vec<String> = self.suggestions.keys().cloned().collect();
            for id in &ids {
                if let (Some(det), Some(game)) = (
                    self.suggestions.remove(id),
                    self.config.games.iter_mut().find(|g| &g.id == id),
                ) {
                    apply_detection(game, &det, include_config);
                }
            }
            self.persist_config();
            self.invalidate_game_caches();
            self.refresh_selected_game_files();
            self.scan_banner = None;
            self.set_status(format!("Updated the save location of {} game(s).", ids.len()), false);
            self.start_sync(ids, None, "Snapshot with new locations".to_string(), false);
        } else if dismiss {
            self.scan_banner = None;
        }
    }

    /// Installed games a scan found while automatic tracking is off.
    fn render_untracked_panel(&mut self, ui: &mut egui::Ui, pal: &JizihubPalette) {
        if self.untracked.is_empty() {
            return;
        }
        let mut track: Vec<String> = Vec::new();
        let mut ignore: Vec<String> = Vec::new();
        let mut track_all = false;
        let mut hide = false;
        glass_frame_2(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                render_badge(ui, pal, "NOT PROTECTED YET", true);
                ui.label(format!("{} installed game(s) are not in your list:", self.untracked.len()));
                if accent_button(ui, pal, !self.busy, "Protect all").clicked() {
                    track_all = true;
                }
                if ui.button("Later").clicked() {
                    hide = true;
                }
            });
            egui::ScrollArea::vertical().id_salt("untracked_scroll").max_height(84.0).show(ui, |ui| {
                for g in &self.untracked {
                    ui.horizontal_wrapped(|ui| {
                        if ui.small_button("Protect").clicked() {
                            track.push(g.id.clone());
                        }
                        if ui.small_button("Never").on_hover_text("Do not offer this game again").clicked() {
                            ignore.push(g.id.clone());
                        }
                        ui.label(egui::RichText::new(&g.name).strong());
                        ui.label(
                            egui::RichText::new(match &g.detection {
                                Some(d) if !g.is_placeholder() => format!("{} · {}", g.provider.display_name(), d.source.display_name()),
                                _ => format!("{} · saves not located yet", g.provider.display_name()),
                            })
                            .small()
                            .color(pal.text_3),
                        );
                    });
                }
            });
        });
        ui.add_space(6.0);

        if track_all {
            track = self.untracked.iter().map(|g| g.id.clone()).collect();
        }
        if hide {
            self.untracked.clear();
        }
        if track.is_empty() && ignore.is_empty() {
            return;
        }
        let mut sync_ids = Vec::new();
        let pending = std::mem::take(&mut self.untracked);
        for g in pending {
            if track.contains(&g.id) {
                if g.enabled && !g.is_placeholder() {
                    sync_ids.push(g.id.clone());
                }
                if !self.config.games.iter().any(|e| e.id == g.id) {
                    self.config.games.push(g);
                }
            } else if ignore.contains(&g.id) {
                if !self.config.ignored_game_ids.contains(&g.id) {
                    self.config.ignored_game_ids.push(g.id);
                }
            } else {
                self.untracked.push(g);
            }
        }
        self.persist_config();
        if !sync_ids.is_empty() {
            self.start_sync(sync_ids, None, "First snapshot of newly protected games".to_string(), false);
        }
    }

    fn render_add_game_panel(&mut self, ui: &mut egui::Ui, pal: &JizihubPalette) {
        glass_frame_2(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.strong("Add a game by hand");
            ui.small(
                egui::RichText::new("For emulators, standalone installs, or anything the scan did not find.")
                    .color(pal.text_3),
            );
            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.add_game_draft.name)
                        .desired_width((ui.available_width() - 220.0).max(160.0)),
                );
                ui.label("Store:");
                egui::ComboBox::from_id_salt("add_game_provider")
                    .selected_text(self.add_game_draft.provider.display_name())
                    .show_ui(ui, |ui| {
                        for &p in Provider::all() {
                            ui.selectable_value(&mut self.add_game_draft.provider, p, p.display_name());
                        }
                    });
            });
            ui.horizontal(|ui| {
                ui.label("Save folder:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.add_game_draft.save_dir)
                        .desired_width((ui.available_width() - 100.0).max(160.0)),
                );
                if ui.button("Browse...").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    self.add_game_draft.save_dir = dir.display().to_string();
                }
            });
            ui.horizontal(|ui| {
                if accent_button(ui, pal, true, "Add game").clicked() {
                    let name = self.add_game_draft.name.trim().to_string();
                    let dir = self.add_game_draft.save_dir.trim().to_string();
                    if name.is_empty() || dir.is_empty() {
                        self.set_status("Enter a name and pick the save folder.", true);
                    } else {
                        let slug = slugify_game_name(&name);
                        let id = format!("{}:{}", self.add_game_draft.provider.slug(), slug);
                        if self.config.games.iter().any(|g| g.id == id) {
                            self.set_status(format!("'{name}' is already in your list."), true);
                        } else {
                            let save_dir = PathBuf::from(dir);
                            let mut entry = GameEntry {
                                id: id.clone(),
                                name,
                                game_slug: slug,
                                provider: self.add_game_draft.provider,
                                save_dir: save_dir.clone(),
                                enabled: true,
                                detection: Some(DetectionInfo {
                                    source: DetectionSource::Manual,
                                    confidence: 100,
                                    reason: "You chose this folder.".to_string(),
                                    database_title: None,
                                }),
                                ..Default::default()
                            };
                            entry.save_dir_template = tokenize_path(&save_dir, &PathContext::for_game(&entry));
                            self.config.games.push(entry);
                            self.add_game_draft = AddGameDraft::default();
                            self.persist_config();
                            self.select_game(id);
                            self.set_status("Game added.", false);
                        }
                    }
                }
                if ui.button("Cancel").clicked() {
                    self.add_game_draft.open = false;
                }
            });
        });
    }

    fn render_game_list(&mut self, ui: &mut egui::Ui, pal: &JizihubPalette) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Search games...")
                    .desired_width(ui.available_width() - 150.0),
            );
            if outline_orange_button(ui, pal, !self.busy, "Find games")
                .on_hover_text("Look for installed games and their save folders")
                .clicked()
            {
                self.start_scan(false);
            }
            if ui.button("+").on_hover_text("Add a game by hand").clicked() {
                self.add_game_draft.open = !self.add_game_draft.open;
            }
        });

        let stores: BTreeSet<Provider> = self.config.games.iter().map(|g| g.provider).collect();
        let attention_count = self
            .config
            .games
            .iter()
            .filter(|g| needs_attention(&self.health_of(g)))
            .count();
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.filter, GameFilter::All, format!("All ({})", self.config.games.len()));
            if attention_count > 0 {
                ui.selectable_value(
                    &mut self.filter,
                    GameFilter::Attention,
                    egui::RichText::new(format!("Needs you ({attention_count})")).color(pal.warn_fg),
                );
            } else if self.filter == GameFilter::Attention {
                self.filter = GameFilter::All;
            }
            if stores.len() > 1 {
                for p in stores {
                    ui.selectable_value(&mut self.filter, GameFilter::Store(p), p.display_name());
                }
            }
        });
        ui.separator();

        let needle = self.search.trim().to_lowercase();
        struct Row {
            id: String,
            name: String,
            store: &'static str,
            health: GameHealth,
            profile: Option<String>,
            label: Option<String>,
            suggestion: bool,
        }
        let mut rows: Vec<Row> = self
            .config
            .games
            .iter()
            .filter(|g| needle.is_empty() || g.name.to_lowercase().contains(&needle))
            .map(|g| Row {
                id: g.id.clone(),
                name: g.name.clone(),
                store: g.provider.display_name(),
                health: self.health_of(g),
                profile: (!g.profiles.is_empty()).then(|| g.profile_name(&g.active_profile)),
                label: g.active_save_label.clone(),
                suggestion: self.suggestions.contains_key(&g.id),
            })
            .filter(|r| match self.filter {
                GameFilter::All => true,
                GameFilter::Attention => needs_attention(&r.health),
                GameFilter::Store(p) => r.store == p.display_name(),
            })
            .collect();
        rows.sort_by(|a, b| {
            needs_attention(&b.health)
                .cmp(&needs_attention(&a.health))
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        let mut clicked: Option<String> = None;
        egui::ScrollArea::vertical()
            .id_salt("games_list_scroll")
            .auto_shrink([false, false])
            .max_height(ui.available_height().max(120.0))
            .show(ui, |ui| {
                if rows.is_empty() {
                    ui.label(
                        egui::RichText::new(if self.config.games.is_empty() {
                            "No games yet. Click \"Find games\" and SaveVault will look for them."
                        } else {
                            "No games match."
                        })
                        .color(pal.text_3),
                    );
                }
                ui.spacing_mut().item_spacing.y = 3.0;
                for row in &rows {
                    let selected = self.selected_game_id.as_deref() == Some(row.id.as_str());
                    let color = health_color(&row.health, pal);
                    let frame = egui::Frame::new()
                        .fill(if selected { pal.accent_soft } else { egui::Color32::TRANSPARENT })
                        .stroke(egui::Stroke::new(
                            1.0,
                            if selected { pal.accent_border } else { egui::Color32::TRANSPARENT },
                        ))
                        .corner_radius(egui::CornerRadius::same(4))
                        .inner_margin(egui::Margin::symmetric(8, 5));
                    let resp = frame
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.spacing_mut().interact_size.y = 14.0;
                            ui.horizontal(|ui| {
                                status_dot(ui, color);
                                ui.add(
                                    egui::Label::new(egui::RichText::new(&row.name).strong().color(pal.text_1))
                                        .truncate()
                                        .selectable(false),
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.add_space(18.0);
                                ui.label(
                                    egui::RichText::new(format!("{} · {}", row.health.label(), row.store))
                                        .small()
                                        .color(color),
                                );
                                if let Some(p) = &row.profile {
                                    ui.label(egui::RichText::new(format!("· {p}")).small().color(pal.text_2));
                                }
                                if let Some(l) = &row.label {
                                    ui.label(egui::RichText::new(format!("· {l}")).small().color(pal.accent_text));
                                }
                                if row.suggestion {
                                    ui.label(egui::RichText::new("· tip").small().color(pal.warn_fg));
                                }
                            });
                        })
                        .response
                        .interact(egui::Sense::click())
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(row.health.explain());
                    if resp.clicked() {
                        clicked = Some(row.id.clone());
                    }
                }
            });
        if let Some(id) = clicked {
            self.select_game(id);
        }
    }

    pub(super) fn render_game_detail(&mut self, ui: &mut egui::Ui, pal: &JizihubPalette) {
        let Some(game) = self.selected_game().cloned() else {
            ui.label(egui::RichText::new("Pick a game on the left.").color(pal.text_3));
            return;
        };
        let health = self.health_of(&game);
        let mut actions: Vec<GameAction> = Vec::new();

        ui.horizontal_wrapped(|ui| {
            ui.heading(&game.name);
            render_badge(ui, pal, game.provider.display_name(), false);
            status_dot(ui, health_color(&health, pal));
            ui.label(egui::RichText::new(health.label()).color(health_color(&health, pal)));
            if !game.profiles.is_empty() {
                render_badge(ui, pal, &format!("Profile: {}", game.profile_name(&game.active_profile)), true);
            }
            if let Some(meta) = &game.retroarch {
                if meta.kbm_enabled {
                    render_badge(ui, pal, "KBM OVERRIDE", true);
                }
                if accent_button(ui, pal, !self.busy, "Launch in RetroArch").clicked() {
                    actions.push(GameAction::LaunchRetroArch);
                }
            }
        });
        let has_retroarch = game.provider == Provider::RetroArch || game.retroarch.is_some();
        if !has_retroarch && self.detail_tab == DetailTab::Controls {
            self.detail_tab = DetailTab::Overview;
        }
        ui.horizontal(|ui| {
            let mut tabs = vec![(DetailTab::Overview, "Overview".to_string())];
            if has_retroarch {
                let kbm_on = game.retroarch.as_ref().is_some_and(|m| m.kbm_enabled);
                tabs.push((
                    DetailTab::Controls,
                    if kbm_on {
                        "Controls & KBM (On)".to_string()
                    } else {
                        "Controls & KBM".to_string()
                    },
                ));
            }
            tabs.extend([
                (DetailTab::Timeline, "Timeline".to_string()),
                (DetailTab::Profiles, format!("Profiles ({})", game.profiles.len() + 1)),
                (DetailTab::Folders, "Folders & Mods".to_string()),
                (DetailTab::Advanced, "More".to_string()),
            ]);
            for (tab, label) in tabs {
                ui.selectable_value(&mut self.detail_tab, tab, label);
            }
        });
        ui.separator();

        egui::ScrollArea::vertical()
            .id_salt("game_detail_scroll")
            .auto_shrink([false, false])
            .max_height(ui.available_height().max(160.0))
            .show(ui, |ui| match self.detail_tab {
                DetailTab::Overview => self.detail_overview(ui, pal, &game, &health, &mut actions),
                DetailTab::Controls => self.detail_controls(ui, pal, &game, &mut actions),
                DetailTab::Timeline => self.detail_timeline(ui, pal, &game, &mut actions),
                DetailTab::Profiles => self.detail_profiles(ui, pal, &game, &mut actions),
                DetailTab::Folders => self.detail_folders(ui, pal, &game, &mut actions),
                DetailTab::Advanced => self.detail_advanced(ui, pal, &game, &mut actions),
            });

        for action in actions {
            self.apply_game_action(&game, action);
        }
    }

    fn with_game_mut(&mut self, id: &str, edit: impl FnOnce(&mut GameEntry)) {
        if let Some(g) = self.config.games.iter_mut().find(|g| g.id == id) {
            edit(g);
            self.persist_config();
        }
    }

    fn set_manual_location(&mut self, id: &str, dir: PathBuf) {
        self.with_game_mut(id, |g| {
            g.save_dir_template = tokenize_path(&dir, &PathContext::for_game(g));
            g.save_dir = dir;
            g.sync_selection = SyncSelectionMode::EntireFolder;
            g.detection = Some(DetectionInfo {
                source: DetectionSource::Manual,
                confidence: 100,
                reason: "You chose this folder.".to_string(),
                database_title: None,
            });
            g.enabled = true;
        });
        self.suggestions.remove(id);
        self.invalidate_game_caches();
        self.refresh_selected_game_files();
    }

    fn apply_game_action(&mut self, game: &GameEntry, action: GameAction) {
        let id = game.id.clone();
        match action {
            GameAction::Sync(label) => {
                self.sync_label_input.clear();
                self.start_sync(vec![id], label, format!("Back up {}", game.name), false);
            }
            GameAction::LaunchRetroArch => self.launch_retroarch_game_action(&id),
            GameAction::AttachRetroArchRom => {
                if let Some(rom) = rfd::FileDialog::new().pick_file() {
                    let ra_root = game.library_root.clone();
                    let (core_path, core_name, sys_name) =
                        crate::retroarch::resolve_core_for_rom_with_root(
                            ra_root.as_deref(),
                            None,
                            None,
                            &rom,
                        )
                        .unwrap_or_else(|| {
                            (
                                crate::retroarch::find_core_library(
                                    ra_root.as_deref(),
                                    "pcsx2_libretro",
                                ),
                                "ParaLLEl PS2".to_string(),
                                "RetroArch".to_string(),
                            )
                        });
                    let new_meta = crate::models::RetroArchGameMeta {
                        rom_path: rom,
                        core_path,
                        core_name,
                        playlist_name: None,
                        system_name: Some(sys_name),
                        kbm_enabled: false,
                        kbm_profile: self.config.default_kbm_profile.clone(),
                        kbm_customized: false,
                    };
                    let include_cfg = self.config.include_config_roots;
                    let db = self.db.clone();
                    self.with_game_mut(&id, |g| {
                        g.retroarch = Some(new_meta);
                        if g.save_dir.as_os_str().is_empty() {
                            let det = crate::detect::detect_game(g, db.as_deref());
                            crate::detect::apply_detection(g, &det, include_cfg);
                        }
                    });
                }
            }
            GameAction::UpdateRetroArchMeta {
                meta,
                commit,
                ensure_bridge,
                status_note,
            } => {
                if let Some(g) = self.config.games.iter_mut().find(|g| g.id == id) {
                    g.retroarch = Some(*meta);
                }
                if commit {
                    self.commit_kbm_game(&id, ensure_bridge);
                } else {
                    self.preview_live_kbm_game(&id);
                }
                if let Some(msg) = status_note {
                    self.set_status(msg, false);
                }
            }
            GameAction::SaveDefaultKbmTemplate(profile) => {
                self.config.default_kbm_profile = *profile;
                self.persist_config();
                self.set_status(
                    "Updated global Default KBM Override settings for new games.",
                    false,
                );
            }
            GameAction::OpenControlsTab => self.detail_tab = DetailTab::Controls,
            GameAction::OpenFolder(path) => open_folder(&path),
            GameAction::OpenEditor(path) => self.open_file_in_editor(&path),
            GameAction::ApplySuggestion => {
                let include_config = self.config.include_config_roots;
                if let Some(det) = self.suggestions.remove(&id) {
                    self.with_game_mut(&id, |g| apply_detection(g, &det, include_config));
                    self.invalidate_game_caches();
                    self.refresh_selected_game_files();
                    self.start_sync(vec![id], None, format!("Snapshot {} from its new location", game.name), false);
                }
            }
            GameAction::DismissSuggestion => {
                self.suggestions.remove(&id);
                // Remember the choice so the tip does not come back on every scan.
                self.with_game_mut(&id, |g| {
                    g.detection = Some(DetectionInfo {
                        source: DetectionSource::Manual,
                        confidence: 100,
                        reason: "You chose to keep this folder.".to_string(),
                        database_title: None,
                    });
                });
            }
            GameAction::ConfirmLocation => {
                self.with_game_mut(&id, |g| {
                    g.detection = Some(DetectionInfo {
                        source: DetectionSource::Manual,
                        confidence: 100,
                        reason: "You confirmed this folder.".to_string(),
                        database_title: None,
                    });
                    g.enabled = true;
                });
                self.start_sync(vec![id], None, format!("First snapshot of {}", game.name), false);
            }
            GameAction::ChooseFolder => {
                let mut dialog = rfd::FileDialog::new();
                if let Some(start) = game
                    .prefix_dir
                    .as_ref()
                    .or(game.install_dir.as_ref())
                    .filter(|p| p.exists())
                {
                    dialog = dialog.set_directory(start);
                }
                if let Some(dir) = dialog.pick_folder() {
                    self.set_manual_location(&id, dir);
                    self.start_sync(vec![id], None, format!("First snapshot of {}", game.name), false);
                }
            }
            GameAction::ResolveConflict { use_cloud } => self.start_resolve_conflict(id, use_cloud),
            GameAction::Restore(commit) => self.start_restore_preview(id, commit),
            GameAction::BranchProfile(commit) => {
                self.new_profile = NewProfileDraft {
                    name: commit
                        .label
                        .clone()
                        .unwrap_or_else(|| format!("From {}", commit.timestamp.split(' ').next().unwrap_or(""))),
                    from_commit: Some((game.active_profile.clone(), commit.commit_hash.clone())),
                    ..Default::default()
                };
                self.detail_tab = DetailTab::Profiles;
            }
            GameAction::SaveLabel(commit, label) => {
                match set_save_state_label(&mut self.config, &id, &commit, &label) {
                    Ok(()) => {
                        self.persist_config();
                        self.editing_label_commit = None;
                        self.commits_loaded_for = None;
                        self.set_status(if label.trim().is_empty() { "Label cleared." } else { "Label saved." }, false);
                    }
                    Err(err) => self.set_status(format!("Could not save the label: {err}"), true),
                }
            }
            GameAction::SwitchProfile(pid) => self.start_switch_profile(id, pid),
            GameAction::DeleteProfile(pid) => self.start_delete_profile(id, pid),
            GameAction::RenameProfile(pid, name) => match rename_profile(&mut self.config, &id, &pid, &name) {
                Ok(()) => {
                    self.persist_config();
                    self.renaming_profile = None;
                    self.profile_cache = None;
                }
                Err(err) => self.set_status(err.to_string(), true),
            },
            GameAction::CreateProfile => self.start_create_profile(id),
            GameAction::Remove => self.pending_remove_game_id = Some(id),
            GameAction::Export => {
                let suggested = format!("{}-saves.zip", game.game_slug);
                if let Some(dst) = rfd::FileDialog::new().set_file_name(suggested).save_file() {
                    self.start_export_zip(id, dst);
                }
            }
            GameAction::Undo(zip) => self.start_undo_from_safety(id, zip),
            GameAction::Rescan => self.start_scan(false),
            GameAction::RefreshFiles => self.refresh_selected_game_files(),
        }
    }

    fn detail_overview(
        &mut self,
        ui: &mut egui::Ui,
        pal: &JizihubPalette,
        game: &GameEntry,
        health: &GameHealth,
        actions: &mut Vec<GameAction>,
    ) {
        ui.label(egui::RichText::new(health.explain()).color(pal.text_2));
        ui.add_space(6.0);

        // Conflict: the one situation that needs a decision.
        if let Some(conflict) = &game.sync_state.conflict {
            egui::Frame::new()
                .fill(pal.accent_soft)
                .stroke(egui::Stroke::new(1.0, pal.warn_fg))
                .corner_radius(egui::CornerRadius::same(5))
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.colored_label(pal.warn_fg, "Two different sets of saves");
                    ui.label(format!(
                        "This PC has new progress, and so does {}{}. Nothing has been overwritten.",
                        conflict
                            .cloud_machine
                            .as_deref()
                            .map(|m| format!("\"{m}\""))
                            .unwrap_or_else(|| "the cloud".to_string()),
                        conflict
                            .cloud_time
                            .as_deref()
                            .map(|t| format!(" (saved {t})"))
                            .unwrap_or_default()
                    ));
                    ui.small("Whichever you pick, the other one is kept: in Safety backups or in the timeline.");
                    ui.horizontal_wrapped(|ui| {
                        if accent_button(ui, pal, !self.busy, "Keep this PC's saves").clicked() {
                            actions.push(GameAction::ResolveConflict { use_cloud: false });
                        }
                        if outline_orange_button(ui, pal, !self.busy, "Use the other saves").clicked() {
                            actions.push(GameAction::ResolveConflict { use_cloud: true });
                        }
                    });
                });
            ui.add_space(8.0);
        }

        // A more precise location is known.
        if let Some(det) = self.suggestions.get(&game.id)
            && let Some(better) = det.saves.first()
        {
            glass_frame_2(pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    render_badge(ui, pal, "TIP", true);
                    ui.strong("A more precise save location is known");
                });
                ui.small(egui::RichText::new(&det.info.reason).color(pal.text_3));
                ui.label(egui::RichText::new(format!("Now:    {}", game.save_dir.display())).monospace().small());
                let include = if better.include.is_empty() {
                    String::new()
                } else {
                    format!("   only: {}", better.include.join(", "))
                };
                ui.label(
                    egui::RichText::new(format!("Better: {}{include}", better.path.display()))
                        .monospace()
                        .small()
                        .color(pal.ok_fg),
                );
                ui.small(format!(
                    "{} file(s), {} there. Existing snapshots stay in the timeline.",
                    better.stats.files,
                    human_bytes(better.stats.bytes)
                ));
                ui.horizontal(|ui| {
                    if accent_button(ui, pal, !self.busy, "Use the precise location").clicked() {
                        actions.push(GameAction::ApplySuggestion);
                    }
                    if ui.button("Keep mine").clicked() {
                        actions.push(GameAction::DismissSuggestion);
                    }
                });
            });
            ui.add_space(8.0);
        }

        // No (trusted) location yet.
        if game.is_placeholder() {
            glass_frame_2(pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.strong(if matches!(health, GameHealth::NotInstalled) {
                    "Saved on another PC"
                } else {
                    "SaveVault does not know where this game saves yet"
                });
                ui.label(
                    game.detection
                        .as_ref()
                        .map(|d| d.reason.clone())
                        .unwrap_or_else(|| "Play it once: SaveVault watches which files change and learns the location.".to_string()),
                );
                ui.horizontal(|ui| {
                    if outline_orange_button(ui, pal, !self.busy, "Choose the folder myself...").clicked() {
                        actions.push(GameAction::ChooseFolder);
                    }
                    if ui.button("Look again").clicked() {
                        actions.push(GameAction::Rescan);
                    }
                });
            });
            return;
        }
        if matches!(health, GameHealth::NeedsReview) {
            glass_frame_2(pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.colored_label(pal.warn_fg, "Is this the right folder?");
                ui.label(egui::RichText::new(game.save_dir.display().to_string()).monospace().small());
                if let Some(d) = &game.detection {
                    ui.small(egui::RichText::new(&d.reason).color(pal.text_3));
                }
                ui.small("Not sure? Just play: SaveVault learns the location from the files that change.");
                ui.horizontal_wrapped(|ui| {
                    if accent_button(ui, pal, !self.busy, "Yes, protect it").clicked() {
                        actions.push(GameAction::ConfirmLocation);
                    }
                    if ui.button("Open folder").clicked() {
                        actions.push(GameAction::OpenFolder(game.save_dir.clone()));
                    }
                    if ui.button("Choose another folder...").clicked() {
                        actions.push(GameAction::ChooseFolder);
                    }
                });
            });
            ui.add_space(8.0);
        }

        // Primary action.
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.sync_label_input)
                    .hint_text("Name this moment (optional), e.g. Before final boss")
                    .desired_width((ui.available_width() - 230.0).max(220.0)),
            );
            if accent_button(ui, pal, !self.busy, "Back up now").clicked() {
                let lbl = self.sync_label_input.trim().to_string();
                actions.push(GameAction::Sync((!lbl.is_empty()).then_some(lbl)));
            }
            if ui.button("Open save folder").clicked() {
                actions.push(GameAction::OpenFolder(game.save_dir.clone()));
            }
        });
        if self.running.contains(&game.id) {
            ui.small(
                egui::RichText::new("The game is running: a snapshot is taken automatically when you close it.")
                    .color(pal.accent_text),
            );
        }
        ui.add_space(8.0);

        if let Some(meta) = &game.retroarch {
            if self.save_states_cache.as_ref().map(|(id, _)| id.as_str()) != Some(game.id.as_str())
            {
                let states = crate::retroarch::list_retroarch_save_states(game);
                self.save_states_cache = Some((game.id.clone(), states));
            }
            let state_files = self
                .save_states_cache
                .as_ref()
                .map(|(_, s)| s.clone())
                .unwrap_or_default();

            glass_frame_2(pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    render_badge(ui, pal, "RETROARCH LAUNCHER", true);
                    ui.strong(format!("Core: {}", meta.core_name));
                    if let Some(sys) = &meta.system_name {
                        ui.label(egui::RichText::new(sys).small().color(pal.text_3));
                    }
                    if meta.kbm_enabled {
                        render_badge(
                            ui,
                            pal,
                            &format!("KBM ON ({:.0}x)", meta.kbm_profile.mouse_sensitivity),
                            true,
                        );
                    } else {
                        render_badge(ui, pal, "GAMEPAD MODE", false);
                    }
                });
                ui.label(
                    egui::RichText::new(format!("ROM: {}", meta.rom_path.display()))
                        .monospace()
                        .small()
                        .color(pal.text_2),
                );
                ui.horizontal_wrapped(|ui| {
                    if accent_button(ui, pal, !self.busy, "Launch Game").clicked() {
                        actions.push(GameAction::LaunchRetroArch);
                    }
                    if outline_orange_button(ui, pal, true, "Keybindings & Sensitivity (Live)")
                        .clicked()
                    {
                        actions.push(GameAction::OpenControlsTab);
                    }
                    if let Some(states_root) =
                        game.extra_roots.iter().find(|r| r.id == "savestates")
                        && ui.button("Open Save States Folder").clicked()
                    {
                        actions.push(GameAction::OpenFolder(states_root.path.clone()));
                    }
                });
                ui.small(
                    egui::RichText::new(format!(
                        "Tracked Save States: {} file(s){}",
                        state_files.len(),
                        if state_files.is_empty() {
                            ""
                        } else {
                            " (backed up & restored together with memory card / SRM saves)"
                        }
                    ))
                    .color(pal.text_3),
                );
                for st in state_files.iter().take(10) {
                    if let Some(name) = st.file_name().and_then(|n| n.to_str()) {
                        ui.label(
                            egui::RichText::new(format!("  • {name}"))
                                .monospace()
                                .small()
                                .color(pal.ok_fg),
                        );
                    }
                }
            });
            ui.add_space(8.0);
        }

        // Facts.
        egui::Grid::new("overview_facts").num_columns(2).spacing([14.0, 5.0]).show(ui, |ui| {
            ui.label(egui::RichText::new("Last snapshot").color(pal.text_3));
            ui.label(match game.last_synced_at {
                Some(t) => format!(
                    "{} ({})",
                    t.with_timezone(&Local).format("%Y-%m-%d %H:%M"),
                    relative_time(t.timestamp())
                ),
                None => "never".to_string(),
            });
            ui.end_row();
            if let Some(st) = &game.last_sync_status {
                ui.label(egui::RichText::new("Last result").color(pal.text_3));
                ui.label(st);
                ui.end_row();
            }
            if let Some(live) = self.live_states.get(&game.id) {
                ui.label(egui::RichText::new("Right now").color(pal.text_3));
                ui.label(format!("{} file(s), {}", live.files, human_bytes(live.bytes)));
                ui.end_row();
            }
            ui.label(egui::RichText::new("Save folder").color(pal.text_3));
            ui.add(
                egui::Label::new(egui::RichText::new(game.save_dir.display().to_string()).monospace().small())
                    .truncate(),
            );
            ui.end_row();
            if let Some(d) = &game.detection {
                ui.label(egui::RichText::new("Found by").color(pal.text_3));
                ui.label(format!("{} · {}", d.source.display_name(), d.reason));
                ui.end_row();
            }
            let extra = game.extra_roots.iter().filter(|r| r.enabled).count();
            if extra > 0 || !game.mod_sources.is_empty() {
                ui.label(egui::RichText::new("Also tracked").color(pal.text_3));
                ui.label(format!(
                    "{extra} more folder(s), {} mod list source(s)",
                    game.mod_sources.iter().filter(|m| m.enabled).count()
                ));
                ui.end_row();
            }
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.strong(format!("Files ({})", self.available_files_cache.len()));
            if ui.small_button("Refresh").clicked() {
                actions.push(GameAction::RefreshFiles);
            }
        });
        if self.cached_files_for_game.as_deref() != Some(game.id.as_str()) {
            actions.push(GameAction::RefreshFiles);
        }
        for f in self.available_files_cache.iter().take(200) {
            ui.horizontal(|ui| {
                if ui.small_button("View").clicked() {
                    actions.push(GameAction::OpenEditor(game.save_dir.join(f)));
                }
                ui.label(egui::RichText::new(f).monospace().small().color(pal.text_2));
            });
        }
        if self.available_files_cache.len() > 200 {
            ui.small(format!("...and {} more.", self.available_files_cache.len() - 200));
        }
    }

    fn detail_controls(
        &mut self,
        ui: &mut egui::Ui,
        pal: &JizihubPalette,
        game: &GameEntry,
        actions: &mut Vec<GameAction>,
    ) {
        let id = game.id.clone();
        let Some(mut meta) = game.retroarch.clone() else {
            glass_frame_2(pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.strong("Configure RetroArch Launch & Controls");
                ui.label(
                    egui::RichText::new(
                        "Attach a ROM file so SaveVault can launch this game directly in RetroArch and manage per-game Mouse & Keyboard bindings.",
                    )
                    .color(pal.text_2),
                );
                if accent_button(ui, pal, true, "Pick ROM File...").clicked() {
                    actions.push(GameAction::AttachRetroArchRom);
                }
            });
            return;
        };

        let mut modified = false;
        let mut discrete_commit = false;
        let mut toggled_kbm_on = false;
        let mut status_note: Option<String> = None;

        glass_frame_2(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                render_badge(ui, pal, &meta.core_name, false);
                if self.running.contains(&id) {
                    render_badge(ui, pal, "LIVE HOT-RELOAD ACTIVE", true);
                }
                if accent_button(ui, pal, !self.busy, "Launch in RetroArch").clicked() {
                    actions.push(GameAction::LaunchRetroArch);
                }
            });
            ui.add_space(4.0);

            if ui
                .checkbox(
                    &mut meta.kbm_enabled,
                    "Enable Mouse & Keyboard (KBM) Override for this game",
                )
                .changed()
            {
                if meta.kbm_enabled && !meta.kbm_customized {
                    meta.kbm_profile = self.config.default_kbm_profile.clone();
                    meta.kbm_customized = true;
                }
                toggled_kbm_on = meta.kbm_enabled;
                modified = true;
                discrete_commit = true;
                status_note = Some(if meta.kbm_enabled {
                    format!("Enabled KBM override for {} (applied live).", game.name)
                } else {
                    format!("Disabled KBM override for {}.", game.name)
                });
            }

            ui.small(
                egui::RichText::new(if self.running.contains(&id) {
                    "This game is currently running: keybinding and mouse sensitivity edits apply live within ~200 ms without restarting."
                } else {
                    "Saved per game. You can edit sensitivity and bindings both before and during gameplay; new games inherit your Default KBM Template from Settings."
                })
                .color(if self.running.contains(&id) {
                    pal.ok_fg
                } else {
                    pal.text_3
                }),
            );

            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button("Reset to Default KBM Template")
                    .on_hover_text("Overwrite this game's KBM bindings & sensitivity from Settings -> Default KBM Template")
                    .clicked()
                {
                    meta.kbm_profile = self.config.default_kbm_profile.clone();
                    meta.kbm_customized = true;
                    modified = true;
                    discrete_commit = true;
                    status_note = Some(format!(
                        "Reset {} KBM profile to your default template.",
                        game.name
                    ));
                }
                if outline_orange_button(ui, pal, true, "Save Current as Default for New Games")
                    .on_hover_text("Use this game's current sensitivity and bindings as the default template when enabling KBM on future games")
                    .clicked()
                {
                    actions.push(GameAction::SaveDefaultKbmTemplate(Box::new(
                        meta.kbm_profile.clone(),
                    )));
                }
            });
        });

        ui.add_space(6.0);
        let editor_outcome = render_kbm_profile_editor(
            ui,
            pal,
            &id,
            Some(&game.name),
            KbmRebindScope::Game(id.clone()),
            &mut meta.kbm_profile,
            &mut self.kbm_rebind,
        );
        if editor_outcome.changed {
            meta.kbm_customized = true;
            modified = true;
        }

        if modified || editor_outcome.commit {
            actions.push(GameAction::UpdateRetroArchMeta {
                meta: Box::new(meta),
                commit: discrete_commit || editor_outcome.commit,
                ensure_bridge: toggled_kbm_on,
                status_note,
            });
        }
    }

    fn detail_timeline(
        &mut self,
        ui: &mut egui::Ui,
        pal: &JizihubPalette,
        game: &GameEntry,
        actions: &mut Vec<GameAction>,
    ) {
        let key = (game.id.clone(), game.active_profile.clone());
        if self.commits_loaded_for.as_ref() != Some(&key) {
            self.refresh_timeline();
        }
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "Every snapshot of \"{}\". Restoring is always undoable.",
                    game.profile_name(&game.active_profile)
                ))
                .color(pal.text_2),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.commit_filter)
                    .hint_text("Search labels...")
                    .desired_width(ui.available_width().max(160.0)),
            );
        });
        ui.add_space(4.0);

        if self.commits.is_empty() {
            ui.label(egui::RichText::new("No snapshots yet. Click \"Back up now\" on the Overview tab.").color(pal.text_3));
            return;
        }

        let needle = self.commit_filter.trim().to_lowercase();
        let commits: Vec<CommitInfo> = self
            .commits
            .iter()
            .filter(|c| {
                needle.is_empty()
                    || c.message.to_lowercase().contains(&needle)
                    || c.label.as_deref().unwrap_or("").to_lowercase().contains(&needle)
                    || c.machine.as_deref().unwrap_or("").to_lowercase().contains(&needle)
            })
            .cloned()
            .collect();

        for (i, c) in commits.iter().enumerate() {
            glass_frame_2(pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(relative_time(c.unix_time))
                            .strong()
                            .color(if i == 0 { pal.accent_text } else { pal.text_1 }),
                    );
                    ui.label(egui::RichText::new(&c.timestamp).small().color(pal.text_3));
                    if i == 0 && needle.is_empty() {
                        render_badge(ui, pal, "LATEST", false);
                    }
                    if let Some(lbl) = &c.label {
                        render_badge(ui, pal, lbl, true);
                    }
                    if let Some(m) = &c.machine {
                        render_badge(ui, pal, m, false);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if outline_orange_button(ui, pal, !self.busy, "Restore...").clicked() {
                            actions.push(GameAction::Restore(c.clone()));
                        }
                        if ui
                            .button("New profile from this")
                            .on_hover_text("Branch off: keep playing from here as a separate character / run")
                            .clicked()
                        {
                            actions.push(GameAction::BranchProfile(c.clone()));
                        }
                        if ui.button("Label").clicked() {
                            if self.editing_label_commit.as_deref() == Some(c.commit_hash.as_str()) {
                                self.editing_label_commit = None;
                            } else {
                                self.editing_label_commit = Some(c.commit_hash.clone());
                                self.editing_label_text = c.label.clone().unwrap_or_default();
                            }
                        }
                    });
                });
                ui.small(
                    egui::RichText::new(format!(
                        "{} · {} file(s) changed · {}",
                        humanize_subject(&c.message),
                        c.changed_files.len(),
                        c.short_hash
                    ))
                    .color(pal.text_3),
                );
                if self.editing_label_commit.as_deref() == Some(c.commit_hash.as_str()) {
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.editing_label_text);
                        if accent_button(ui, pal, true, "Save").clicked() {
                            actions.push(GameAction::SaveLabel(c.commit_hash.clone(), self.editing_label_text.clone()));
                        }
                        if ui.button("Clear").clicked() {
                            actions.push(GameAction::SaveLabel(c.commit_hash.clone(), String::new()));
                        }
                    });
                }
            });
            ui.add_space(4.0);
        }
    }

    fn detail_profiles(
        &mut self,
        ui: &mut egui::Ui,
        pal: &JizihubPalette,
        game: &GameEntry,
        actions: &mut Vec<GameAction>,
    ) {
        ui.label(
            egui::RichText::new(
                "Profiles are separate sets of saves for the same game: another character, a modded run, \
                 someone else on this PC. Switching swaps the files in the game's save folder; each profile \
                 keeps its own timeline.",
            )
            .color(pal.text_2),
        );
        ui.add_space(6.0);
        if game.is_placeholder() {
            ui.label("Profiles become available once the game's save folder is known.");
            return;
        }

        if self.profile_cache.as_ref().map(|(id, _)| id.as_str()) != Some(game.id.as_str()) {
            self.profile_cache = Some((game.id.clone(), profile_summaries(&self.config, &game.id)));
        }
        let summaries = self.profile_cache.as_ref().map(|(_, s)| s.clone()).unwrap_or_default();
        let running = self.running.contains(&game.id);

        for p in &summaries {
            glass_frame_2(pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.strong(&p.name);
                    if p.active {
                        render_badge(ui, pal, "IN USE", true);
                    }
                    if !p.note.is_empty() {
                        ui.label(egui::RichText::new(&p.note).small().color(pal.text_3));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !p.active {
                            if accent_button(ui, pal, !self.busy && !running, "Switch to this")
                                .on_hover_text("Saves your current profile first, then puts this profile's files in place")
                                .clicked()
                            {
                                actions.push(GameAction::SwitchProfile(p.id.clone()));
                            }
                            if !p.id.is_empty() && ui.button("Delete").clicked() {
                                actions.push(GameAction::DeleteProfile(p.id.clone()));
                            }
                        }
                        if ui.button("Rename").clicked() {
                            self.renaming_profile = Some((p.id.clone(), p.name.clone()));
                        }
                    });
                });
                ui.small(
                    egui::RichText::new(match &p.last_snapshot {
                        Some(c) => format!("Last snapshot {} ({})", relative_time(c.unix_time), c.timestamp),
                        None => "No snapshots yet: starts fresh.".to_string(),
                    })
                    .color(pal.text_3),
                );
                if let Some((rid, text)) = self.renaming_profile.as_mut()
                    && *rid == p.id
                {
                    let mut done = false;
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(text);
                        if ui.button("Save name").clicked() {
                            actions.push(GameAction::RenameProfile(rid.clone(), text.clone()));
                        }
                        if ui.button("Cancel").clicked() {
                            done = true;
                        }
                    });
                    if done {
                        self.renaming_profile = None;
                    }
                }
            });
            ui.add_space(4.0);
        }
        if running {
            ui.small(egui::RichText::new("Close the game to switch profiles.").color(pal.warn_fg));
        }

        ui.add_space(8.0);
        glass_frame_2(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.strong("New profile");
            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_profile.name)
                        .hint_text("e.g. Mage, Hardcore run, Sam")
                        .desired_width(ui.available_width()),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Note:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_profile.note)
                        .hint_text("optional")
                        .desired_width(ui.available_width()),
                );
            });
            if let Some((_, commit)) = &self.new_profile.from_commit {
                let short = commit[..commit.len().min(7)].to_string();
                let mut clear = false;
                ui.horizontal(|ui| {
                    render_badge(ui, pal, &format!("Starts from snapshot {short}"), true);
                    if ui.small_button("x").clicked() {
                        clear = true;
                    }
                });
                if clear {
                    self.new_profile.from_commit = None;
                }
            } else {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.new_profile.copy_current, false, "Fresh start (no saves)");
                    ui.radio_value(&mut self.new_profile.copy_current, true, "Copy of my current saves");
                });
            }
            ui.checkbox(&mut self.new_profile.switch_after, "Switch to it right away");
            if accent_button(
                ui,
                pal,
                !self.busy && !self.new_profile.name.trim().is_empty(),
                "Create profile",
            )
            .clicked()
            {
                actions.push(GameAction::CreateProfile);
            }
        });
    }

    fn detail_folders(
        &mut self,
        ui: &mut egui::Ui,
        pal: &JizihubPalette,
        game: &GameEntry,
        actions: &mut Vec<GameAction>,
    ) {
        let id = game.id.clone();

        // Primary folder.
        ui.strong("Save folder");
        let mut path_text = game.save_dir.display().to_string();
        ui.horizontal(|ui| {
            let resp = ui.add(egui::TextEdit::singleline(&mut path_text).desired_width(ui.available_width() - 170.0));
            if resp.lost_focus() && path_text.trim() != game.save_dir.display().to_string() && !path_text.trim().is_empty() {
                self.set_manual_location(&id, PathBuf::from(path_text.trim()));
            }
            if ui.button("Browse...").clicked() {
                actions.push(GameAction::ChooseFolder);
            }
            if ui.button("Open").clicked() {
                actions.push(GameAction::OpenFolder(game.save_dir.clone()));
            }
        });
        if let Some(t) = &game.save_dir_template {
            ui.small(
                egui::RichText::new(format!("Portable location: {t}"))
                    .monospace()
                    .color(pal.text_3),
            )
            .on_hover_text("This is how another PC (or a reinstall) finds the same folder.");
        }

        let is_entire = matches!(game.sync_selection, SyncSelectionMode::EntireFolder);
        ui.horizontal(|ui| {
            if ui.radio(is_entire, "Everything in the folder").clicked() && !is_entire {
                self.with_game_mut(&id, |g| g.sync_selection = SyncSelectionMode::EntireFolder);
            }
            if ui.radio(!is_entire, "Only selected files / patterns").clicked() && is_entire {
                let all = self.available_files_cache.clone();
                self.with_game_mut(&id, |g| g.sync_selection = SyncSelectionMode::SelectedFiles(all));
            }
        });
        if let SyncSelectionMode::SelectedFiles(selected) = &game.sync_selection {
            let patterns: Vec<&String> = selected
                .iter()
                .filter(|s| crate::pattern::has_glob_chars(s))
                .collect();
            if !patterns.is_empty() {
                ui.small(format!(
                    "Patterns: {}",
                    patterns.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                ));
            }
            let mut set: BTreeSet<String> = selected.iter().cloned().collect();
            let mut changed = false;
            let mut new_excludes: Vec<String> = Vec::new();
            let matcher = crate::pattern::PatternSet::new(selected);
            egui::ScrollArea::vertical()
                .id_salt("selected_files_checklist")
                .max_height(140.0)
                .show(ui, |ui| {
                    for f in &self.available_files_cache {
                        let mut checked = matcher.matches(f);
                        if ui.checkbox(&mut checked, egui::RichText::new(f).monospace().small()).changed() {
                            if checked {
                                set.insert(f.clone());
                            } else if !set.remove(f) {
                                // Covered by a pattern: exclude it explicitly instead.
                                new_excludes.push(f.clone());
                            }
                            changed = true;
                        }
                    }
                });
            if changed {
                let list: Vec<String> = set.into_iter().collect();
                self.with_game_mut(&id, |g| {
                    g.sync_selection = SyncSelectionMode::SelectedFiles(list);
                    for f in new_excludes {
                        if !g.exclude_globs.contains(&f) {
                            g.exclude_globs.push(f);
                        }
                    }
                });
                self.exclude_buffer = None;
            }
        }

        // Extra folders.
        ui.add_space(10.0);
        ui.strong("More folders for this game");
        ui.small(
            egui::RichText::new(
                "Settings, a second save location, or mod configuration. \"Same for every profile\" folders are not swapped when you switch profile.",
            )
            .color(pal.text_3),
        );
        let mut remove_root: Option<usize> = None;
        let mut root_edits: Vec<(usize, bool, bool)> = Vec::new();
        for (i, root) in game.extra_roots.iter().enumerate() {
            glass_frame_2(pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    let mut enabled = root.enabled;
                    let mut shared = root.shared_across_profiles;
                    let c1 = ui.checkbox(&mut enabled, "").changed();
                    render_badge(ui, pal, root.kind.display_name(), root.enabled);
                    ui.label(&root.label);
                    let c2 = ui.checkbox(&mut shared, "Same for every profile").changed();
                    if c1 || c2 {
                        root_edits.push((i, enabled, shared));
                    }
                    if ui.small_button("Open").clicked() {
                        actions.push(GameAction::OpenFolder(root.path.clone()));
                    }
                    if ui.small_button("Remove").clicked() {
                        remove_root = Some(i);
                    }
                });
                let include = if root.include.is_empty() {
                    String::new()
                } else {
                    format!("   only: {}", root.include.join(", "))
                };
                ui.label(
                    egui::RichText::new(format!("{}{include}", root.path.display()))
                        .monospace()
                        .small()
                        .color(pal.text_3),
                );
            });
        }
        if !root_edits.is_empty() || remove_root.is_some() {
            self.with_game_mut(&id, |g| {
                for (i, enabled, shared) in root_edits {
                    if let Some(r) = g.extra_roots.get_mut(i) {
                        r.enabled = enabled;
                        r.shared_across_profiles = shared;
                    }
                }
                if let Some(i) = remove_root {
                    g.extra_roots.remove(i);
                }
            });
        }
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("add_root_kind")
                .selected_text(self.add_root_kind.display_name())
                .show_ui(ui, |ui| {
                    for k in [RootKind::Saves, RootKind::Config, RootKind::Mods] {
                        ui.selectable_value(&mut self.add_root_kind, k, k.display_name());
                    }
                });
            if ui.button("Add folder...").clicked()
                && let Some(dir) = rfd::FileDialog::new().pick_folder()
            {
                let kind = self.add_root_kind;
                self.with_game_mut(&id, |g| {
                    let base = match kind {
                        RootKind::Saves => "saves",
                        RootKind::Config => "config",
                        RootKind::Mods => "mods",
                    };
                    let mut rid = base.to_string();
                    let mut n = 2;
                    while rid == crate::models::PRIMARY_ROOT_ID || g.extra_roots.iter().any(|r| r.id == rid) {
                        rid = format!("{base}_{n}");
                        n += 1;
                    }
                    let template = tokenize_path(&dir, &PathContext::for_game(g));
                    g.extra_roots.push(SaveRoot {
                        id: rid,
                        label: dir
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| kind.display_name().to_string()),
                        kind,
                        path: dir,
                        enabled: true,
                        shared_across_profiles: kind != RootKind::Saves,
                        template,
                        ..Default::default()
                    });
                });
            }
        });

        // Excludes.
        ui.add_space(10.0);
        ui.strong("Never back up");
        ui.small(
            egui::RichText::new("One pattern per line, e.g. autosave*  or  Screenshots  or  **/*.log")
                .color(pal.text_3),
        );
        if self.exclude_buffer.as_ref().map(|(g, _)| g.as_str()) != Some(id.as_str()) {
            self.exclude_buffer = Some((id.clone(), game.exclude_globs.join("\n")));
        }
        if let Some((_, text)) = self.exclude_buffer.as_mut() {
            let resp = ui.add(egui::TextEdit::multiline(text).desired_rows(2).desired_width(f32::INFINITY));
            if resp.lost_focus() {
                let list: Vec<String> = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect();
                if list != game.exclude_globs {
                    self.with_game_mut(&id, |g| g.exclude_globs = list);
                }
            }
        }

        // Mods.
        ui.add_space(10.0);
        ui.strong("Mod list");
        ui.small(
            egui::RichText::new(
                "SaveVault records which mods are installed with every snapshot and warns you before restoring a save made with a different setup. Only names are recorded, mod files are not copied.",
            )
            .color(pal.text_3),
        );
        let mut remove_mod: Option<usize> = None;
        let mut mod_toggles: Vec<(usize, bool)> = Vec::new();
        for (i, m) in game.mod_sources.iter().enumerate() {
            ui.horizontal_wrapped(|ui| {
                let mut enabled = m.enabled;
                if ui.checkbox(&mut enabled, &m.label).changed() {
                    mod_toggles.push((i, enabled));
                }
                ui.label(egui::RichText::new(m.path.display().to_string()).monospace().small().color(pal.text_3));
                if ui.small_button("Remove").clicked() {
                    remove_mod = Some(i);
                }
            });
        }
        if game.mod_sources.is_empty() {
            ui.small("No mod folders found for this game.");
        } else if let Some(fp) = scan_mod_fingerprint_cached(self, game) {
            ui.small(format!("{fp} mod entr(ies) installed right now."));
        }
        if !mod_toggles.is_empty() || remove_mod.is_some() {
            self.with_game_mut(&id, |g| {
                for (i, e) in mod_toggles {
                    if let Some(m) = g.mod_sources.get_mut(i) {
                        m.enabled = e;
                    }
                }
                if let Some(i) = remove_mod {
                    g.mod_sources.remove(i);
                }
            });
            self.mods_cache = None;
        }
        ui.horizontal(|ui| {
            if ui.button("Add mods folder...").clicked()
                && let Some(dir) = rfd::FileDialog::new().pick_folder()
            {
                self.with_game_mut(&id, |g| {
                    g.mod_sources.push(ModSource {
                        label: dir
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "Mods".to_string()),
                        path: dir,
                        kind: ModSourceKind::Folder,
                        enabled: true,
                    });
                });
                self.mods_cache = None;
            }
            if ui
                .button("Add load order file...")
                .on_hover_text("plugins.txt, loadorder.txt, a Mod Organizer modlist.txt, ...")
                .clicked()
                && let Some(file) = rfd::FileDialog::new().pick_file()
            {
                self.with_game_mut(&id, |g| {
                    g.mod_sources.push(ModSource {
                        label: "Load order".to_string(),
                        path: file,
                        kind: ModSourceKind::ListFile,
                        enabled: true,
                    });
                });
                self.mods_cache = None;
            }
        });
    }

    fn detail_advanced(
        &mut self,
        ui: &mut egui::Ui,
        pal: &JizihubPalette,
        game: &GameEntry,
        actions: &mut Vec<GameAction>,
    ) {
        let id = game.id.clone();
        let global_rate = self.config.global_sync_rate;

        let mut enabled = game.enabled;
        if ui.checkbox(&mut enabled, "Protect this game").changed() {
            self.with_game_mut(&id, |g| g.enabled = enabled);
        }

        ui.horizontal(|ui| {
            ui.label("Check for changes:");
            let mut rate = game.sync_rate_override;
            let current = match rate {
                None => format!("Like everything else ({})", global_rate.display_label()),
                Some(r) => r.display_label(),
            };
            let mut changed = false;
            egui::ComboBox::from_id_salt("game_sync_rate")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    changed |= ui
                        .selectable_value(&mut rate, None, format!("Like everything else ({})", global_rate.display_label()))
                        .changed();
                    for (val, text) in [
                        (SyncRate::OnStartup, "When SaveVault starts"),
                        (SyncRate::Manual, "Only when I click"),
                        (SyncRate::EveryMinutes(5), "Every 5 min"),
                        (SyncRate::EveryMinutes(15), "Every 15 min"),
                        (SyncRate::EveryMinutes(60), "Every hour"),
                    ] {
                        changed |= ui.selectable_value(&mut rate, Some(val), text).changed();
                    }
                });
            if changed {
                self.with_game_mut(&id, |g| g.sync_rate_override = rate);
            }
        });

        ui.horizontal(|ui| {
            ui.label("When I close the game:");
            let mut choice = game.backup_on_exit;
            let global = self.config.backup_on_game_exit;
            let text = |c: Option<bool>| match c {
                None => format!("Like everything else ({})", if global { "snapshot" } else { "nothing" }),
                Some(true) => "Always take a snapshot".to_string(),
                Some(false) => "Do nothing".to_string(),
            };
            let mut changed = false;
            egui::ComboBox::from_id_salt("game_exit_backup")
                .selected_text(text(choice))
                .show_ui(ui, |ui| {
                    for c in [None, Some(true), Some(false)] {
                        changed |= ui.selectable_value(&mut choice, c, text(c)).changed();
                    }
                });
            if changed {
                self.with_game_mut(&id, |g| g.backup_on_exit = choice);
            }
        });

        let multi = find_multi_provider_groups(&self.config.games);
        if let Some(stores) = multi.get(&game.game_slug) {
            let names: Vec<&str> = stores.iter().map(|p| p.display_name()).collect();
            let mut linked = game.sync_across_providers;
            if ui
                .checkbox(&mut linked, format!("Share saves between my copies on {}", names.join(" and ")))
                .on_hover_text("The newest saves are copied to the other store's save folder on every snapshot")
                .changed()
            {
                let slug = game.game_slug.clone();
                for g in &mut self.config.games {
                    if g.game_slug == slug {
                        g.sync_across_providers = linked;
                    }
                }
                self.persist_config();
            }
        }

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Export saves as .zip...").on_hover_text("To share with a friend or keep elsewhere").clicked() {
                actions.push(GameAction::Export);
            }
            if ui.button("Detect location again").clicked() {
                self.with_game_mut(&id, |g| {
                    if g.detection.as_ref().is_some_and(|d| d.source == DetectionSource::Manual) {
                        g.detection = None;
                    }
                });
                actions.push(GameAction::Rescan);
            }
            if ui.button("Stop tracking this game...").clicked() {
                actions.push(GameAction::Remove);
            }
        });

        ui.add_space(10.0);
        ui.strong("Safety backups");
        ui.small(
            egui::RichText::new(
                "Before SaveVault replaces files in the save folder (restore, profile switch, newer cloud saves) it keeps a zip of what was there.",
            )
            .color(pal.text_3),
        );
        if self.safety_cache.as_ref().map(|(g, _)| g.as_str()) != Some(id.as_str()) {
            self.safety_cache = Some((id.clone(), list_safety_backups(&self.config, game)));
        }
        let backups = self.safety_cache.as_ref().map(|(_, b)| b.clone()).unwrap_or_default();
        if backups.is_empty() {
            ui.small("None yet.");
        }
        for b in backups.iter().take(20) {
            ui.horizontal_wrapped(|ui| {
                ui.label(b.created.format("%Y-%m-%d %H:%M").to_string());
                ui.label(egui::RichText::new(format!("{} · {}", b.reason, human_bytes(b.bytes))).small().color(pal.text_3));
                if ui
                    .small_button("Put back")
                    .on_hover_text("Copies these files back into the save folder (a new safety backup of the current files is made first)")
                    .clicked()
                {
                    actions.push(GameAction::Undo(b.path.clone()));
                }
                if ui.small_button("Show file").clicked() {
                    actions.push(GameAction::OpenFolder(b.path.clone()));
                }
            });
        }
        ui.small(
            egui::RichText::new(format!("Folder: {}", safety_root(&self.config).display()))
                .monospace()
                .color(pal.text_3),
        );
    }
}

/// Counts installed mod entries for the selected game, cached until the game changes.
fn scan_mod_fingerprint_cached(app: &mut SaveGameManagerApp, game: &GameEntry) -> Option<usize> {
    if app.mods_cache.as_ref().map(|(id, _)| id.as_str()) != Some(game.id.as_str()) {
        let count = scan_mod_fingerprint(&game.mod_sources).map(|f| f.entries.len());
        app.mods_cache = Some((game.id.clone(), count));
    }
    app.mods_cache.as_ref().and_then(|(_, c)| *c)
}
