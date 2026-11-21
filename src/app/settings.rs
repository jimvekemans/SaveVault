use super::*;

fn sign_in_field_frame(
    pal: &JizihubPalette,
    section_highlighted: bool,
    field_missing: bool,
) -> egui::Frame {
    if field_missing {
        egui::Frame::new()
            .fill(pal.accent_soft)
            .stroke(egui::Stroke::new(1.5, pal.warn_fg))
            .corner_radius(egui::CornerRadius::same(4))
            .inner_margin(egui::Margin::symmetric(8, 6))
    } else if section_highlighted {
        egui::Frame::new()
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::new(1.0, egui::Color32::TRANSPARENT))
            .corner_radius(egui::CornerRadius::same(4))
            .inner_margin(egui::Margin::symmetric(8, 6))
    } else {
        egui::Frame::NONE
    }
}

fn render_github_username_row(
    ui: &mut egui::Ui,
    pal: &JizihubPalette,
    github_username: &mut String,
    section_highlighted: bool,
    missing_flag: bool,
    changed: &mut bool,
) {
    let username_missing = missing_flag && github_username.trim().is_empty();
    sign_in_field_frame(pal, section_highlighted, username_missing).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label("GitHub Username:");
            let reserve = if username_missing { 95.0 } else { 0.0 };
            if ui
                .add(
                    egui::TextEdit::singleline(github_username)
                        .desired_width((ui.available_width() - reserve).max(180.0)),
                )
                .changed()
            {
                *changed = true;
            }
            if username_missing {
                render_badge(ui, pal, "REQUIRED", true);
            }
        });
    });
}

impl SaveGameManagerApp {
    pub(super) fn render_libraries_section(&mut self, ui: &mut egui::Ui, pal: &JizihubPalette) {
        let platform = Platform::current();
        glass_frame_1(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.strong(format!("Where to look for games ({})", platform.display_name()));
                if outline_orange_button(ui, pal, !self.busy, "Find games now").clicked()
                {
                    self.scan_libraries_into_config();
                }
            });
            ui.label(
                egui::RichText::new(
                    "The usual launcher folders are checked automatically. Add a folder here if your games live somewhere unusual (another drive, a custom library).",
                )
                .color(pal.text_2),
            );
            ui.separator();

            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
            let mut changed = false;

            egui::ScrollArea::vertical()
                .id_salt("libraries_scroll")
                .auto_shrink([false, false])
                .max_height(280.0)
                .show(ui, |ui| {
                    for lib in &mut self.config.provider_libraries {
                        glass_frame_2(pal).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                if ui
                                    .checkbox(&mut lib.enabled, lib.provider.display_name())
                                    .changed()
                                {
                                    changed = true;
                                }
                                if ui.button("Add Library Folder...").clicked()
                                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                                    && !lib.library_paths.contains(&dir)
                                {
                                    lib.library_paths.push(dir);
                                    changed = true;
                                }
                                if ui.button("Reset to Platform Defaults").clicked() {
                                    lib.library_paths =
                                        default_library_paths(lib.provider, platform, &home);
                                    changed = true;
                                }
                            });

                            let mut remove_idx = None;
                            for (idx, path) in lib.library_paths.iter_mut().enumerate() {
                                ui.horizontal(|ui| {
                                    let exists = path.exists();
                                    render_badge(
                                        ui,
                                        pal,
                                        if exists { "FOUND" } else { "MISSING" },
                                        exists,
                                    );
                                    let mut path_str = path.display().to_string();
                                    if ui
                                        .add(
                                            egui::TextEdit::singleline(&mut path_str)
                                                .desired_width((ui.available_width() - 90.0).max(180.0)),
                                        )
                                        .changed()
                                    {
                                        *path = PathBuf::from(path_str);
                                        changed = true;
                                    }
                                    if ui.button("Remove").clicked() {
                                        remove_idx = Some(idx);
                                    }
                                });
                            }
                            if let Some(i) = remove_idx {
                                lib.library_paths.remove(i);
                                changed = true;
                            }
                        });
                        ui.add_space(6.0);
                    }
                });

            if changed {
                self.persist_config();
            }
        });
    }

    pub(super) fn render_settings_tab(&mut self, ui: &mut egui::Ui, pal: &JizihubPalette) {
        let mut changed = false;
        let mut refresh_ssh = false;
        let mut apply_theme = false;
        let mut toggle_autostart: Option<bool> = None;
        let mut pending_status: Option<(String, bool)> = None;

        egui::ScrollArea::vertical()
            .id_salt("settings_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                glass_frame_1(pal).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.strong("General");
                    ui.add_space(4.0);

                    ui.horizontal(|ui| {
                        ui.label("Theme:");
                        for (mode, label) in [
                            (ThemeMode::Light, "Light"),
                            (ThemeMode::Dark, "Dark"),
                        ] {
                            let active = self.config.theme_mode == mode;
                            let btn = egui::Button::new(
                                egui::RichText::new(label)
                                    .small()
                                    .strong()
                                    .color(if active { pal.accent_contrast } else { pal.text_2 }),
                            )
                            .fill(if active { pal.accent } else { pal.glass_2 })
                            .stroke(egui::Stroke::new(
                                1.0,
                                if active {
                                    pal.accent
                                } else {
                                    pal.border_strong
                                },
                            ))
                            .corner_radius(egui::CornerRadius::same(4));
                            if ui
                                .add(btn)
                                .on_hover_text("Switch between Light and Dark theme")
                                .clicked()
                            {
                                self.config.theme_mode = mode;
                                changed = true;
                                apply_theme = true;
                            }
                        }
                    });

                    if ui
                        .checkbox(
                            &mut self.config.sync_enabled,
                            "Keep a cloud copy (sync snapshots with GitHub or your own remote)",
                        )
                        .on_hover_text("Snapshots are always kept on this PC. With this on, they are also uploaded, and saves from your other PCs are brought here.")
                        .changed()
                    {
                        changed = true;
                        pending_status = Some((
                            if self.config.sync_enabled {
                                if self.config.remote_configured() {
                                    "Cloud sync is on.".to_string()
                                } else {
                                    "Cloud sync is on. Connect GitHub (or a custom remote) below to start uploading.".to_string()
                                }
                            } else {
                                "Cloud sync is off. Snapshots stay on this PC.".to_string()
                            },
                            false,
                        ));
                    }

                    if ui
                        .checkbox(
                            &mut self.config.run_on_system_startup,
                            "Start SaveVault when I log in (minimized)",
                        )
                        .changed()
                    {
                        changed = true;
                        toggle_autostart = Some(self.config.run_on_system_startup);
                    }

                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        ui.label("GitHub private repository name:");
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut self.config.repo_name)
                                    .desired_width((ui.available_width() - 380.0).max(220.0)),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                        render_external_link(
                            ui,
                            pal,
                            "Create a new repository on GitHub (https://github.com/new)",
                            "https://github.com/new",
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Local repository clone path:");
                        let mut repo_str = self.config.local_repo_path.display().to_string();
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut repo_str)
                                    .desired_width((ui.available_width() - 95.0).max(200.0)),
                            )
                            .changed()
                        {
                            self.config.local_repo_path = PathBuf::from(repo_str);
                            changed = true;
                        }
                        if ui.button("Browse...").clicked()
                            && let Some(dir) = rfd::FileDialog::new().pick_folder()
                        {
                            self.config.local_repo_path = dir;
                            changed = true;
                        }
                    });
                    if ui
                        .checkbox(
                            &mut self.config.separate_folder_per_provider,
                            "Make a separate folder in the repo per provider, per game (e.g. epic_games/metro_exodus and steam/metro_exodus)",
                        )
                        .changed()
                    {
                        changed = true;
                    }

                    ui.horizontal(|ui| {
                        ui.label("Check all games for changes:");
                        egui::ComboBox::from_id_salt("global_sync_rate_combo")
                            .selected_text(self.config.global_sync_rate.display_label())
                            .show_ui(ui, |ui| {
                                for (rate, label) in [
                                    (SyncRate::OnStartup, "When SaveVault starts"),
                                    (SyncRate::Manual, "Only when I click"),
                                    (
                                        SyncRate::EveryMinutes(self.custom_minutes_buffer),
                                        "Every N Minutes",
                                    ),
                                ] {
                                    if ui
                                        .selectable_value(
                                            &mut self.config.global_sync_rate,
                                            rate,
                                            label,
                                        )
                                        .changed()
                                    {
                                        changed = true;
                                    }
                                }
                            });
                        if let SyncRate::EveryMinutes(mins) = &mut self.config.global_sync_rate
                            && ui
                                .add(egui::DragValue::new(mins).range(1..=1440).suffix(" min"))
                                .changed()
                        {
                            self.custom_minutes_buffer = *mins;
                            changed = true;
                        }
                    });
                });

                ui.add_space(8.0);
                self.render_retroarch_default_kbm_settings(ui, pal, &mut changed);

                ui.add_space(8.0);
                self.render_protection_settings(ui, pal, &mut changed);

                ui.add_space(8.0);
                glass_frame_1(pal).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.strong("Large Save Splitting (GitHub & LFS Limit Protection)");
                    if ui
                        .checkbox(
                            &mut self.config.split_large_saves,
                            "Split saves that may be too big for GitHub / GitHub LFS (with user warning)",
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    ui.horizontal(|ui| {
                        ui.label("Warning & split threshold (MB):");
                        if ui
                            .add(
                                egui::DragValue::new(&mut self.config.lfs_warning_threshold_mb)
                                    .range(1..=2048)
                                    .suffix(" MB"),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                        ui.label("Chunk size (MB):");
                        if ui
                            .add(
                                egui::DragValue::new(&mut self.config.chunk_size_mb)
                                    .range(1..=95)
                                    .suffix(" MB"),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                    });
                    ui.small(
                        egui::RichText::new(
                            "GitHub blocks single files >100 MB (and GitHub LFS limits files to 2 GB). Splitting breaks oversized saves into verifiable SHA-256 chunks that reassemble transparently on rollback.",
                        )
                        .color(pal.text_3),
                    );
                });

                ui.add_space(8.0);
                let highlight_cloud = self.settings_highlight == Some(SettingsHighlight::CloudSignIn)
                    || self.cloud_needs_sign_in;
                let needs_credentials = !self.config.remote_configured();
                let missing_github_creds = highlight_cloud && needs_credentials;
                let mut cloud_frame = glass_frame_1(pal);
                if highlight_cloud {
                    let stroke_color = if needs_credentials {
                        pal.warn_fg
                    } else {
                        pal.accent
                    };
                    cloud_frame = cloud_frame.stroke(egui::Stroke::new(2.0, stroke_color));
                }
                let cloud_frame_resp = cloud_frame.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.strong("Cloud copy");
                        if highlight_cloud {
                            render_badge(
                                ui,
                                pal,
                                if needs_credentials {
                                    "SIGN-IN NEEDED"
                                } else {
                                    "READY TO CONNECT"
                                },
                                true,
                            );
                        }
                    });
                    if highlight_cloud {
                        ui.add_space(4.0);
                        egui::Frame::new()
                            .fill(pal.accent_soft)
                            .stroke(egui::Stroke::new(
                                1.0,
                                if needs_credentials {
                                    pal.warn_fg
                                } else {
                                    pal.accent_border
                                },
                            ))
                            .corner_radius(egui::CornerRadius::same(5))
                            .inner_margin(egui::Margin::same(10))
                            .show(ui, |ui| {
                                let msg = if needs_credentials {
                                    "Your local repository has a cloud remote, but SaveVault has no working sign-in for it. Fill in the highlighted GitHub sign-in fields below (or set a custom remote) to resume cloud sync."
                                } else {
                                    "Sign-in details are filled in. Click Connect below to verify your cloud repository."
                                };
                                ui.label(
                                    egui::RichText::new(msg).color(if needs_credentials {
                                        pal.warn_fg
                                    } else {
                                        pal.accent_text
                                    }),
                                );
                            });
                        ui.add_space(4.0);
                    }
                    ui.small(
                        egui::RichText::new(
                            "Optional. Snapshots are stored in a private git repository; connect GitHub below, or point SaveVault at any git remote you own.",
                        )
                        .color(pal.text_3),
                    );
                    let mut custom = self.config.custom_remote_url.clone().unwrap_or_default();
                    ui.horizontal(|ui| {
                        ui.label("Custom remote (instead of GitHub):");
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut custom)
                                    .hint_text("git URL or folder, e.g. /mnt/nas/saves.git")
                                    .desired_width(ui.available_width().max(260.0)),
                            )
                            .changed()
                        {
                            self.config.custom_remote_url =
                                if custom.trim().is_empty() { None } else { Some(custom.trim().to_string()) };
                            changed = true;
                        }
                    });
                    ui.separator();
                    ui.strong("GitHub");

                    let mut choice = match self.config.auth {
                        AuthMethod::ExistingSshKey { .. } => AuthKindChoice::ExistingSshKey,
                        AuthMethod::NewSshKey { .. } => AuthKindChoice::NewSshKey,
                        AuthMethod::PersonalAccessToken { .. } => AuthKindChoice::PersonalAccessToken,
                    };
                    let prev_choice = choice;

                    ui.horizontal(|ui| {
                        ui.radio_value(
                            &mut choice,
                            AuthKindChoice::ExistingSshKey,
                            "Use Existing SSH Key",
                        );
                        ui.radio_value(
                            &mut choice,
                            AuthKindChoice::NewSshKey,
                            "Create New SSH Key",
                        );
                        ui.radio_value(
                            &mut choice,
                            AuthKindChoice::PersonalAccessToken,
                            "Personal Access Token (PAT)",
                        );
                    });

                    if choice != prev_choice {
                        let ssh_dir = dirs::home_dir()
                            .unwrap_or_else(|| PathBuf::from("."))
                            .join(".ssh");
                        let existing_user = self.config.auth.github_username().to_string();
                        let existing_pat = self.config.auth.pat_token().map(str::to_string);
                        self.config.auth = match choice {
                            AuthKindChoice::ExistingSshKey => AuthMethod::ExistingSshKey {
                                private_key_path: ssh_dir.join("id_ed25519"),
                                public_key_path: Some(ssh_dir.join("id_ed25519.pub")),
                                passphrase: String::new(),
                                github_username: existing_user,
                                pat_for_api: existing_pat,
                            },
                            AuthKindChoice::NewSshKey => {
                                let priv_path = ssh_dir.join("save_game_manager_ed25519");
                                AuthMethod::NewSshKey {
                                    public_key_path: public_key_path_for(&priv_path),
                                    private_key_path: priv_path,
                                    passphrase: String::new(),
                                    github_username: existing_user,
                                    pat_for_api: existing_pat,
                                    key_generated: false,
                                }
                            }
                            AuthKindChoice::PersonalAccessToken => AuthMethod::PersonalAccessToken {
                                token: existing_pat.unwrap_or_default(),
                                github_username: existing_user,
                            },
                        };
                        changed = true;
                        refresh_ssh = true;
                    }

                    ui.separator();
                    let is_busy = self.busy;
                    let pub_preview_snapshot = self.ssh_pub_key_preview.clone();
                    let detected_keys = detect_existing_ssh_keys();
                    let mut generated_ok: Option<String> = None;
                    let mut upload_key_request: Option<(String, String)> = None;
                    let mut verify_token_request: Option<String> = None;
                    let mut copied_pub_key = false;

                    pub(super) fn render_ssh_key_preview(
                        ui: &mut egui::Ui,
                        pal: &JizihubPalette,
                        pub_preview: &str,
                        copied_pub_key: &mut bool,
                    ) {
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Public Key Preview:");
                            render_external_link(
                                ui,
                                pal,
                                "Add or manage SSH keys on GitHub",
                                "https://github.com/settings/keys",
                            );
                        });
                        if pub_preview.is_empty() {
                            ui.small(
                                egui::RichText::new(
                                    "No matching .pub file found yet at the selected path.",
                                )
                                .color(pal.text_3),
                            );
                        } else {
                            ui.horizontal(|ui| {
                                let mut preview = pub_preview.to_string();
                                ui.add(
                                    egui::TextEdit::multiline(&mut preview)
                                        .desired_rows(2)
                                        .desired_width((ui.available_width() - 140.0).max(240.0)),
                                );
                                if ui.button("Copy Public Key").clicked() {
                                    ui.ctx().copy_text(pub_preview.to_string());
                                    *copied_pub_key = true;
                                }
                            });
                        }
                    }

                    match &mut self.config.auth {
                        AuthMethod::ExistingSshKey {
                            private_key_path,
                            public_key_path,
                            github_username,
                            pat_for_api,
                            ..
                        } => {
                            ui.small(
                                egui::RichText::new(
                                    "Uses your existing local SSH private key over `git@github.com:<user>/<repo>.git`.",
                                )
                                .color(pal.text_3),
                            );
                            render_github_username_row(
                                ui,
                                pal,
                                github_username,
                                highlight_cloud,
                                missing_github_creds,
                                &mut changed,
                            );
                            let key_missing = missing_github_creds && !private_key_path.exists();
                            sign_in_field_frame(pal, highlight_cloud, key_missing).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                if !detected_keys.is_empty() {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.small(
                                            egui::RichText::new("Detected local keys:")
                                                .color(pal.text_3),
                                        );
                                        for candidate in &detected_keys {
                                            let label = candidate
                                                .file_name()
                                                .and_then(|n| n.to_str())
                                                .unwrap_or("key");
                                            let selected = *private_key_path == *candidate;
                                            if ui.selectable_label(selected, label).clicked() {
                                                *private_key_path = candidate.clone();
                                                *public_key_path =
                                                    Some(public_key_path_for(private_key_path));
                                                changed = true;
                                                refresh_ssh = true;
                                            }
                                        }
                                    });
                                }
                                ui.horizontal_wrapped(|ui| {
                                    ui.label("SSH Private Key:");
                                    let mut p = private_key_path.display().to_string();
                                    let reserve = if key_missing { 210.0 } else { 95.0 };
                                    if ui
                                        .add(
                                            egui::TextEdit::singleline(&mut p)
                                                .desired_width((ui.available_width() - reserve).max(180.0)),
                                        )
                                        .changed()
                                    {
                                        *private_key_path = PathBuf::from(p);
                                        *public_key_path = Some(public_key_path_for(private_key_path));
                                        changed = true;
                                        refresh_ssh = true;
                                    }
                                    if ui.button("Browse...").clicked()
                                        && let Some(file) = rfd::FileDialog::new().pick_file()
                                    {
                                        *private_key_path = file;
                                        *public_key_path = Some(public_key_path_for(private_key_path));
                                        changed = true;
                                        refresh_ssh = true;
                                    }
                                    if key_missing {
                                        render_badge(ui, pal, "MISSING KEY", true);
                                    }
                                });
                            });
                            render_ssh_key_preview(
                                ui,
                                pal,
                                &pub_preview_snapshot,
                                &mut copied_pub_key,
                            );
                            ui.add_space(4.0);
                            ui.collapsing(
                                "Optional: Personal Access Token for automatic private repository creation",
                                |ui| {
                                    ui.small(
                                        egui::RichText::new(
                                            "Only needed if SaveVault should create the private GitHub repository via API; if you created it on github.com/new yourself, leave this blank.",
                                        )
                                        .color(pal.text_3),
                                    );
                                    let mut pat_buf = pat_for_api.clone().unwrap_or_default();
                                    ui.horizontal(|ui| {
                                        ui.label("Optional PAT:");
                                        if ui
                                            .add(
                                                egui::TextEdit::singleline(&mut pat_buf)
                                                    .password(true)
                                                    .desired_width(ui.available_width().max(200.0)),
                                            )
                                            .changed()
                                        {
                                            *pat_for_api = if pat_buf.is_empty() {
                                                None
                                            } else {
                                                Some(pat_buf)
                                            };
                                            changed = true;
                                        }
                                    });
                                },
                            );
                        }
                        AuthMethod::NewSshKey {
                            private_key_path,
                            public_key_path,
                            passphrase,
                            github_username,
                            pat_for_api,
                            key_generated,
                        } => {
                            ui.small(
                                egui::RichText::new(
                                    "Generates a dedicated Ed25519 SSH keypair for SaveVault and helps you register it on GitHub.",
                                )
                                .color(pal.text_3),
                            );
                            render_github_username_row(
                                ui,
                                pal,
                                github_username,
                                highlight_cloud,
                                missing_github_creds,
                                &mut changed,
                            );
                            let new_key_missing =
                                missing_github_creds && !(*key_generated && private_key_path.exists());
                            sign_in_field_frame(pal, highlight_cloud, new_key_missing).show(
                                ui,
                                |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        ui.label("New Private Key Path:");
                                        let mut p = private_key_path.display().to_string();
                                        if ui
                                            .add(
                                                egui::TextEdit::singleline(&mut p)
                                                    .desired_width((ui.available_width() - 150.0).max(180.0)),
                                            )
                                            .changed()
                                        {
                                            *private_key_path = PathBuf::from(p);
                                            *public_key_path = public_key_path_for(private_key_path);
                                            changed = true;
                                            refresh_ssh = true;
                                        }
                                        if ui.button("Choose Location...").clicked()
                                            && let Some(file) = rfd::FileDialog::new()
                                                .set_file_name("save_game_manager_ed25519")
                                                .save_file()
                                        {
                                            *private_key_path = file;
                                            *public_key_path = public_key_path_for(private_key_path);
                                            changed = true;
                                            refresh_ssh = true;
                                        }
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("Passphrase (optional):");
                                        if ui
                                            .add(
                                                egui::TextEdit::singleline(passphrase)
                                                    .password(true)
                                                    .desired_width(ui.available_width().max(180.0)),
                                            )
                                            .changed()
                                        {
                                            changed = true;
                                        }
                                    });
                                    let mut pat_buf = pat_for_api.clone().unwrap_or_default();
                                    ui.horizontal(|ui| {
                                        ui.label("Optional PAT (to upload key & create repo):");
                                        if ui
                                            .add(
                                                egui::TextEdit::singleline(&mut pat_buf)
                                                    .password(true)
                                                    .desired_width(ui.available_width().max(180.0)),
                                            )
                                            .changed()
                                        {
                                            *pat_for_api = if pat_buf.is_empty() {
                                                None
                                            } else {
                                                Some(pat_buf)
                                            };
                                            changed = true;
                                        }
                                    });

                                    let priv_clone = private_key_path.clone();
                                    let pass_clone = passphrase.clone();
                                    let pat_clone = pat_for_api.clone();

                                    ui.horizontal_wrapped(|ui| {
                                        if outline_orange_button(
                                            ui,
                                            pal,
                                            true,
                                            "Generate New SSH Keypair",
                                        )
                                        .clicked()
                                        {
                                            match generate_ssh_keypair(&priv_clone, &pass_clone) {
                                                Ok(pub_key) => {
                                                    *key_generated = true;
                                                    generated_ok = Some(pub_key);
                                                    changed = true;
                                                }
                                                Err(err) => {
                                                    generated_ok = Some(format!("ERR:{err}"));
                                                }
                                            }
                                        }
                                        if *key_generated || priv_clone.is_file() {
                                            render_badge(ui, pal, "Key Ready", true);
                                        } else if new_key_missing {
                                            render_badge(ui, pal, "KEY NEEDED", true);
                                        }
                                        if let Some(tok) =
                                            pat_clone.as_deref().filter(|s| !s.trim().is_empty())
                                            && !pub_preview_snapshot.is_empty()
                                            && ui
                                                .add_enabled(
                                                    !is_busy,
                                                    egui::Button::new("Upload Key to GitHub (via PAT)"),
                                                )
                                                .clicked()
                                        {
                                            upload_key_request =
                                                Some((tok.to_string(), pub_preview_snapshot.clone()));
                                        }
                                    });
                                },
                            );
                            render_ssh_key_preview(
                                ui,
                                pal,
                                &pub_preview_snapshot,
                                &mut copied_pub_key,
                            );
                        }
                        AuthMethod::PersonalAccessToken {
                            token,
                            github_username,
                        } => {
                            ui.small(
                                egui::RichText::new(
                                    "Connects over HTTPS using your Personal Access Token (`https://github.com/<user>/<repo>.git`). No SSH keys are required.",
                                )
                                .color(pal.text_3),
                            );
                            render_external_link(
                                ui,
                                pal,
                                "Generate a Personal Access Token on GitHub (repo scope)",
                                "https://github.com/settings/tokens/new",
                            );
                            render_github_username_row(
                                ui,
                                pal,
                                github_username,
                                highlight_cloud,
                                missing_github_creds,
                                &mut changed,
                            );
                            let pat_missing = missing_github_creds && token.trim().is_empty();
                            sign_in_field_frame(pal, highlight_cloud, pat_missing).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal_wrapped(|ui| {
                                    ui.label("Personal Access Token (PAT):");
                                    let reserve = if pat_missing { 330.0 } else { 230.0 };
                                    if ui
                                        .add(
                                            egui::TextEdit::singleline(token)
                                                .password(true)
                                                .desired_width((ui.available_width() - reserve).max(180.0)),
                                        )
                                        .changed()
                                    {
                                        changed = true;
                                    }
                                    let tok_clone = token.clone();
                                    if outline_orange_button(
                                        ui,
                                        pal,
                                        !is_busy,
                                        "Verify Token & Fetch Username",
                                    )
                                    .clicked()
                                    {
                                        verify_token_request = Some(tok_clone);
                                    }
                                    if pat_missing {
                                        render_badge(ui, pal, "REQUIRED", true);
                                    }
                                });
                            });
                        }
                    }

                    if copied_pub_key {
                        self.set_status("Copied public SSH key to clipboard.", false);
                    }
                    if let Some(res) = generated_ok {
                        if let Some(err) = res.strip_prefix("ERR:") {
                            self.set_status(err.to_string(), true);
                        } else {
                            self.ssh_pub_key_preview = res;
                            self.set_status("Generated new Ed25519 SSH keypair.", false);
                        }
                    }
                    if let Some((tok, pub_key)) = upload_key_request {
                        self.start_upload_ssh_key(tok, pub_key);
                    }
                    if let Some(tok) = verify_token_request {
                        self.start_verify_token(tok);
                    }

                    ui.add_space(8.0);
                    let repo_btn_label = if self.config.custom_remote().is_some() {
                        "Test connection to the custom remote".to_string()
                    } else {
                        format!(
                            "Connect: make sure private repository '{}' exists",
                            if self.config.repo_name.is_empty() {
                                "save_game_manager"
                            } else {
                                &self.config.repo_name
                            }
                        )
                    };
                    ui.horizontal_wrapped(|ui| {
                        if accent_button(ui, pal, !self.busy, &repo_btn_label).clicked() {
                            self.start_ensure_repo();
                        }
                        if ui
                            .add_enabled(!self.busy, egui::Button::new("Check the cloud for games from my other PCs"))
                            .clicked()
                        {
                            self.start_pull_from_remote();
                        }
                    });
                });
                if self.scroll_to_settings_highlight {
                    cloud_frame_resp
                        .response
                        .scroll_to_me(Some(egui::Align::Center));
                    self.scroll_to_settings_highlight = false;
                }
            });

        if apply_theme {
            theme::apply_jizihub_theme(ui.ctx(), self.config.theme_mode);
        }
        if changed {
            self.persist_config();
        }
        if refresh_ssh {
            self.refresh_ssh_preview();
        }
        if let Some(enable) = toggle_autostart {
            match set_system_autostart(enable) {
                Ok(path) => {
                    let msg = if enable {
                        format!("Enabled startup launch entry at {}", path.display())
                    } else {
                        format!("Disabled startup launch entry ({})", path.display())
                    };
                    self.set_status(msg, false);
                }
                Err(err) => {
                    self.config.run_on_system_startup = !enable;
                    self.persist_config();
                    self.set_status(format!("Failed to update system startup setting: {err}"), true);
                }
            }
        }
        if let Some((msg, is_err)) = pending_status {
            self.set_status(msg, is_err);
        }
    }

    pub(super) fn render_retroarch_default_kbm_settings(
        &mut self,
        ui: &mut egui::Ui,
        pal: &JizihubPalette,
        changed: &mut bool,
    ) {
        glass_frame_1(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.strong("RetroArch Launcher & Default KBM Override Template");
                render_badge(ui, pal, "NEW GAMES", false);
            });
            ui.small(
                egui::RichText::new(
                    "Default Mouse & Keyboard settings applied automatically when you enable KBM overrides on a new RetroArch game.",
                )
                .color(pal.text_3),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("RetroArch binary / command:");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut self.config.retroarch_binary)
                            .desired_width((ui.available_width() - 120.0).max(180.0)),
                    )
                    .changed()
                {
                    *changed = true;
                }
                if ui.button("Reset").clicked() {
                    self.config.retroarch_binary = crate::models::default_retroarch_binary();
                    *changed = true;
                }
            });

            ui.add_space(4.0);
            egui::CollapsingHeader::new("Default KBM Override Settings (Sensitivity & Keybindings)")
                .id_salt("default_kbm_template_header")
                .default_open(false)
                .show(ui, |ui| {
                    if ui.button("Reset Template to FPS Defaults").clicked() {
                        self.config.default_kbm_profile = crate::models::KbmProfile::default();
                        *changed = true;
                    }
                    let kbm_outcome = render_kbm_profile_editor(
                        ui,
                        pal,
                        "default_kbm_template",
                        None,
                        KbmRebindScope::DefaultTemplate,
                        &mut self.config.default_kbm_profile,
                        &mut self.kbm_rebind,
                    );
                    if kbm_outcome.commit {
                        self.kbm_dirty_default = false;
                        *changed = true;
                    } else if kbm_outcome.changed {
                        self.kbm_dirty_default = true;
                    }
                });
        });
    }

    pub(super) fn render_protection_settings(&mut self, ui: &mut egui::Ui, pal: &JizihubPalette, changed: &mut bool) {
        glass_frame_1(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.strong("Automatic protection");
            ui.horizontal(|ui| {
                ui.label("This PC is called:");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut self.config.machine_name)
                            .desired_width((ui.available_width() * 0.45).max(220.0)),
                    )
                    .on_hover_text("Shown next to snapshots so you can tell your computers apart")
                    .changed()
                {
                    *changed = true;
                }
            });
            if ui
                .checkbox(
                    &mut self.config.backup_on_game_exit,
                    "Take a snapshot when I close a game",
                )
                .changed()
            {
                *changed = true;
            }
            ui.horizontal(|ui| {
                if ui
                    .checkbox(
                        &mut self.config.safety_backups,
                        "Keep a safety zip before replacing files in a save folder",
                    )
                    .changed()
                {
                    *changed = true;
                }
                ui.label("keep the last");
                if ui
                    .add(egui::DragValue::new(&mut self.config.safety_backup_keep).range(1..=100))
                    .changed()
                {
                    *changed = true;
                }
                ui.label("per game");
            });
            ui.horizontal(|ui| {
                ui.label("If two PCs both have new saves for a game:");
                egui::ComboBox::from_id_salt("conflict_policy")
                    .selected_text(self.config.conflict_policy.display_name())
                    .show_ui(ui, |ui| {
                        for p in [ConflictPolicy::Ask, ConflictPolicy::PreferLocal, ConflictPolicy::PreferCloud] {
                            if ui
                                .selectable_value(&mut self.config.conflict_policy, p, p.display_name())
                                .changed()
                            {
                                *changed = true;
                            }
                        }
                    });
            });
            ui.small(
                egui::RichText::new("Either way the other version is kept (in the timeline or as a safety zip).")
                    .color(pal.text_3),
            );
        });

        ui.add_space(8.0);
        glass_frame_1(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.strong("Finding games and saves");
            if ui
                .checkbox(
                    &mut self.config.auto_track_new_games,
                    "Protect newly installed games automatically",
                )
                .changed()
            {
                *changed = true;
            }
            if ui
                .checkbox(
                    &mut self.config.include_config_roots,
                    "Also back up game settings (graphics, key bindings) for new games",
                )
                .changed()
            {
                *changed = true;
            }
            if ui
                .checkbox(
                    &mut self.config.auto_update_detection_db,
                    "Keep the save location database up to date (weekly download from github.com/mtkennerly/ludusavi-manifest)",
                )
                .changed()
            {
                *changed = true;
            }
            ui.horizontal_wrapped(|ui| {
                match &self.db {
                    Some(db) => {
                        render_badge(ui, pal, &format!("{} GAMES KNOWN", db.len()), true);
                        if let Some(t) = db.meta.fetched_at {
                            ui.small(
                                egui::RichText::new(format!("database from {}", t.format("%Y-%m-%d")))
                                    .color(pal.text_3),
                            );
                        }
                    }
                    None => {
                        render_badge(ui, pal, "DATABASE NOT LOADED", false);
                        ui.small(
                            egui::RichText::new("Steam Cloud folders, smart scan and learn-while-playing still work.")
                                .color(pal.text_3),
                        );
                    }
                }
                if ui.add_enabled(!self.busy, egui::Button::new("Update now")).clicked() {
                    self.start_db_update();
                }
            });
            ui.add_space(4.0);
            ui.label("Never back up, for any game (one pattern per line):");
            let mut text = self.config.global_exclude_globs.join("\n");
            if ui
                .add(
                    egui::TextEdit::multiline(&mut text)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .hint_text("e.g. *.log\nScreenshots"),
                )
                .changed()
            {
                self.config.global_exclude_globs = text
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect();
                *changed = true;
            }
            ui.add_space(6.0);
            egui::CollapsingHeader::new("Launcher folders")
                .id_salt("launcher_folders")
                .show(ui, |ui| {
                    self.render_libraries_section(ui, pal);
                });
        });
    }
}
