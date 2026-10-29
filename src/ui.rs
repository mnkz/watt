use std::sync::atomic::Ordering;

use chrono::NaiveDate;
use eframe::egui;
use eframe::egui::text::LayoutJob;
use eframe::egui::{
    Align2, Button, Color32, CornerRadius, FontFamily, FontId, Frame, Margin, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, TextEdit, TextFormat, Vec2,
};

use crate::WattApp;
use crate::model::{
    Bucket, Period, Provider, RATE_LABEL, Snapshot, day_span, fmt_span, period_bounds,
};

pub const BG: Color32 = Color32::from_rgb(0, 0, 0);
pub const TEXT: Color32 = Color32::from_rgb(244, 244, 242);
pub const MUTED: Color32 = Color32::from_rgb(138, 138, 136);
pub const LINE: Color32 = Color32::from_rgb(38, 38, 38);
pub const STROKE: Color32 = Color32::from_rgb(42, 42, 42);

struct ModelUse {
    model: String,
    tokens: u64,
    micros: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

pub(crate) struct ProviderUse {
    provider: Provider,
    tokens: u64,
    micros: u64,
    models: Vec<ModelUse>,
}

pub(crate) struct PeriodView {
    pub(crate) tokens: u64,
    pub(crate) api_usd: f64,
    pub(crate) abo_usd: f64,
    pub(crate) days: i64,
    pub(crate) rows: Vec<ProviderUse>,
}

impl WattApp {
    pub(crate) fn draw(&mut self, ui: &mut egui::Ui) {
        let frame = Frame::new()
            .fill(BG)
            .stroke(Stroke::new(1.0, STROKE))
            .inner_margin(Margin::same(18))
            .corner_radius(CornerRadius::ZERO);
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            self.header(ui);
            self.period_row(ui);
            self.progress(ui);
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                    let today = chrono::Local::now().date_naive();
                    let view = period_view(
                        &self.snap,
                        self.period,
                        today,
                        self.custom_start,
                        self.custom_end,
                        &self.abos,
                    );
                    let scanning = self.shared.scanning.load(Ordering::Relaxed);
                    let cold = scanning && self.snap.buckets.is_empty();
                    if cold {
                        self.cold_scan(ui);
                    } else {
                        self.hero(ui, &view);
                        ui.add_space(6.0);
                        self.providers(ui, &view);
                    }
                    ui.add_space(14.0);
                    self.subscriptions(ui);
                    ui.add_space(12.0);
                    self.footer(ui, scanning);
                });
        });
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let width = (ui.available_width() - 28.0).max(0.0);
            let (rect, resp) =
                ui.allocate_exact_size(Vec2::new(width, 22.0), Sense::click_and_drag());
            let job = LayoutJob::single_section(
                "WATT".to_string(),
                TextFormat {
                    font_id: FontId::new(12.0, FontFamily::Proportional),
                    extra_letter_spacing: 2.6,
                    color: TEXT,
                    ..Default::default()
                },
            );
            let painter = ui.painter();
            let galley = painter.layout_job(job);
            let pos = Pos2::new(rect.left(), rect.center().y - galley.size().y * 0.5);
            painter.galley(pos, galley, TEXT);
            if resp.drag_started() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            resp.on_hover_cursor(egui::CursorIcon::Grab);

            let close = Button::new(RichText::new("×").size(18.0).color(MUTED))
                .frame(false)
                .fill(BG);
            if ui
                .add_sized(Vec2::new(28.0, 22.0), close)
                .on_hover_text("Réduit dans la barre des tâches")
                .clicked()
            {
                self.hide(ui.ctx());
            }
        });
    }

    fn period_row(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
            for period in Period::all() {
                if chip(ui, period.label(), self.period == period) {
                    self.period = period;
                    changed = true;
                }
            }
        });
        if self.period == Period::Custom {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                changed |= self.date_field(ui, true);
                ui.label(RichText::new("–").size(12.0).color(MUTED));
                changed |= self.date_field(ui, false);
            });
        }
        let today = chrono::Local::now().date_naive();
        let (start, end) = period_bounds(self.period, today, self.custom_start, self.custom_end);
        ui.label(RichText::new(fmt_span(start, end)).size(12.0).color(MUTED));
        if changed {
            self.persist();
        }
    }

    fn date_field(&mut self, ui: &mut egui::Ui, start: bool) -> bool {
        let text = if start {
            &mut self.start_text
        } else {
            &mut self.end_text
        };
        let resp = ui.add(
            TextEdit::singleline(text)
                .desired_width(108.0)
                .hint_text("AAAA-MM-JJ")
                .font(FontId::new(13.0, FontFamily::Proportional)),
        );
        if !resp.changed() {
            return false;
        }
        let Ok(day) = NaiveDate::parse_from_str(text, "%Y-%m-%d") else {
            return false;
        };
        if start {
            self.custom_start = day;
        } else {
            self.custom_end = day;
        }
        true
    }

    fn progress(&self, ui: &mut egui::Ui) {
        let scanning = self.shared.scanning.load(Ordering::Relaxed);
        if !scanning {
            hairline(ui);
            return;
        }
        let done = self.shared.progress_done.load(Ordering::Relaxed);
        let total = self.shared.progress_total.load(Ordering::Relaxed).max(1);
        ui.label(
            RichText::new(format!("Lecture des journaux   {done} / {total}"))
                .size(11.0)
                .color(MUTED),
        );
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 2.0), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::ZERO, LINE);
        let w = rect.width() * (done as f32 / total as f32).clamp(0.0, 1.0);
        if w > 0.0 {
            painter.rect_filled(
                Rect::from_min_size(rect.min, Vec2::new(w, rect.height())),
                CornerRadius::ZERO,
                TEXT,
            );
        }
        ui.add_space(4.0);
    }

    fn cold_scan(&self, ui: &mut egui::Ui) {
        ui.add_space(28.0);
        ui.label(RichText::new("Lecture des journaux").size(22.0).color(TEXT));
        let done = self.shared.progress_done.load(Ordering::Relaxed);
        let total = self.shared.progress_total.load(Ordering::Relaxed);
        ui.label(
            RichText::new(format!("{done} / {total}"))
                .size(13.0)
                .color(MUTED),
        );
        ui.add_space(8.0);
        ui.label(
            RichText::new(
                "La première lecture traverse plusieurs Go. Le cache rend les suivantes immédiates.",
            )
            .size(12.0)
            .color(MUTED),
        );
    }

    fn hero(&self, ui: &mut egui::Ui, view: &PeriodView) {
        ui.add_space(8.0);
        let big = if view.tokens == 0 {
            "—".to_string()
        } else {
            fmt_tokens_short(view.tokens)
        };
        ui.label(RichText::new(big).size(42.0).color(TEXT));
        let api = if view.tokens == 0 {
            "—".to_string()
        } else {
            fmt_money(view.api_usd)
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(RichText::new("valeur API").size(12.0).color(MUTED));
            ui.label(RichText::new(api).size(13.0).color(TEXT));
        });
        ui.add_space(8.0);
        let max = view.api_usd.max(view.abo_usd);
        meter(ui, "API", view.api_usd, max, TEXT);
        meter(ui, "Abo", view.abo_usd, max, MUTED);
        ui.add_space(2.0);
        ui.label(RichText::new(ratio_line(view)).size(13.0).color(TEXT));
        let jour = if view.days > 1 { "jours" } else { "jour" };
        ui.label(
            RichText::new(format!(
                "Abonnements au prorata · {} {jour} / 30",
                view.days
            ))
            .size(11.0)
            .color(MUTED),
        );
    }

    fn providers(&mut self, ui: &mut egui::Ui, view: &PeriodView) {
        ui.add_space(8.0);
        for row in &view.rows {
            hairline(ui);
            ui.add_space(2.0);
            let open = self.expanded == Some(row.provider);
            let tokens = if row.provider == Provider::Agy || row.tokens == 0 {
                "—".to_string()
            } else {
                fmt_tokens_short(row.tokens)
            };
            let money = if row.provider == Provider::Agy || row.tokens == 0 {
                "—".to_string()
            } else {
                fmt_money(row.micros as f64 / 1_000_000.0)
            };
            if provider_line(ui, row.provider.label(), &tokens, &money, false)
                && row.provider != Provider::Agy
            {
                self.expanded = if open { None } else { Some(row.provider) };
            }
            if row.provider == Provider::Agy {
                if let Some(note) = row.provider.detail_note() {
                    note_line(ui, note);
                }
                continue;
            }
            if !open {
                continue;
            }
            if row.models.is_empty() {
                note_line(ui, "Aucun jeton sur la période.");
            }
            for model in &row.models {
                let name = model_label(row.provider, &model.model);
                let money = fmt_money(model.micros as f64 / 1_000_000.0);
                model_line(ui, &name, &fmt_tokens_full(model.tokens), &money);
                let detail = format!(
                    "entrée {}   ·   cache lu {}   ·   cache écrit {}   ·   sortie {}",
                    fmt_tokens_full(model.input),
                    fmt_tokens_full(model.cache_read),
                    fmt_tokens_full(model.cache_write),
                    fmt_tokens_full(model.output),
                );
                note_line(ui, &detail);
            }
            if let Some(note) = row.provider.detail_note() {
                note_line(ui, note);
            }
            ui.add_space(4.0);
        }
        hairline(ui);
    }

    fn subscriptions(&mut self, ui: &mut egui::Ui) {
        if section_toggle(ui, "Abonnements par mois", self.abos_open) {
            self.abos_open = !self.abos_open;
        }
        if !self.abos_open {
            let line = Provider::all()
                .iter()
                .map(|p| format!("{} {}", p.label(), fmt_money(self.abos[idx(*p)])))
                .collect::<Vec<_>>()
                .join("   ·   ");
            ui.label(RichText::new(line).size(11.0).color(MUTED));
            return;
        }
        ui.label(
            RichText::new("Montants saisis par vous, en dollars par mois.")
                .size(11.0)
                .color(MUTED),
        );
        let mut changed = false;
        for provider in Provider::all() {
            ui.add_space(2.0);
            ui.label(RichText::new(provider.label()).size(13.0).color(TEXT));
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                let i = idx(provider);
                let mut value = self.abos[i];
                let resp = ui.add(
                    egui::DragValue::new(&mut value)
                        .range(0.0..=9_999.0)
                        .speed(1.0)
                        .max_decimals(2)
                        .suffix(" $"),
                );
                if resp.changed() {
                    self.abos[i] = value;
                    changed = true;
                }
                for (name, amount) in provider.presets() {
                    if chip(ui, name, (self.abos[i] - amount).abs() < 0.01) {
                        self.abos[i] = *amount;
                        changed = true;
                    }
                }
            });
        }
        if changed {
            self.persist();
        }
    }

    fn footer(&mut self, ui: &mut egui::Ui, scanning: bool) {
        ui.label(
            RichText::new(format!("tarifs publics · {RATE_LABEL}"))
                .size(11.0)
                .color(MUTED),
        );
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let when = match &self.last_read {
                Some(t) => t.format("%H:%M").to_string(),
                None => "—".to_string(),
            };
            let state = if scanning {
                "lecture en cours".to_string()
            } else {
                format!("lu à {when} · toutes les 60 s")
            };
            ui.label(RichText::new(state).size(11.0).color(MUTED));
            if !scanning && text_button(ui, "actualiser") {
                self.refresh();
            }
        });
    }
}

pub fn period_view(
    snap: &Snapshot,
    period: Period,
    today: NaiveDate,
    custom_start: NaiveDate,
    custom_end: NaiveDate,
    abos: &[f64; 5],
) -> PeriodView {
    let (start, end) = period_bounds(period, today, custom_start, custom_end);
    let days = day_span(start, end).max(1);
    let mut rows: Vec<ProviderUse> = Provider::all()
        .into_iter()
        .map(|provider| ProviderUse {
            provider,
            tokens: 0,
            micros: 0,
            models: Vec::new(),
        })
        .collect();
    for bucket in &snap.buckets {
        let Some(day) = bucket.date() else { continue };
        if day < start || day > end {
            continue;
        }
        let row = &mut rows[idx(bucket.provider)];
        add_model(row, bucket);
    }
    for row in &mut rows {
        row.models
            .sort_by(|a, b| b.micros.cmp(&a.micros).then(b.tokens.cmp(&a.tokens)));
    }
    let tokens = rows.iter().map(|r| r.tokens).sum();
    let micros: u64 = rows.iter().map(|r| r.micros).sum();
    let monthly: f64 = abos.iter().sum();
    PeriodView {
        tokens,
        api_usd: micros as f64 / 1_000_000.0,
        abo_usd: monthly * (days as f64) / 30.0,
        days,
        rows,
    }
}

fn add_model(row: &mut ProviderUse, bucket: &Bucket) {
    row.tokens = row.tokens.saturating_add(bucket.tokens());
    row.micros = row.micros.saturating_add(bucket.cost_micros);
    if let Some(model) = row.models.iter_mut().find(|m| m.model == bucket.model) {
        model.tokens = model.tokens.saturating_add(bucket.tokens());
        model.micros = model.micros.saturating_add(bucket.cost_micros);
        model.input = model.input.saturating_add(bucket.input);
        model.output = model.output.saturating_add(bucket.output);
        model.cache_read = model.cache_read.saturating_add(bucket.cache_read);
        model.cache_write = model.cache_write.saturating_add(bucket.cache_write);
        return;
    }
    row.models.push(ModelUse {
        model: bucket.model.clone(),
        tokens: bucket.tokens(),
        micros: bucket.cost_micros,
        input: bucket.input,
        output: bucket.output,
        cache_read: bucket.cache_read,
        cache_write: bucket.cache_write,
    });
}

fn idx(provider: Provider) -> usize {
    Provider::all()
        .iter()
        .position(|p| *p == provider)
        .unwrap_or(0)
}

fn model_label(provider: Provider, model: &str) -> String {
    if provider == Provider::Cursor {
        "tarif indicatif".to_string()
    } else {
        model.to_string()
    }
}

fn ratio_line(view: &PeriodView) -> String {
    if view.abo_usd <= 0.0 {
        return "Abonnement à 0 $.".to_string();
    }
    let ratio = view.api_usd / view.abo_usd;
    format!("{}× l’abonnement", fmt_ratio(ratio))
}

fn chip(ui: &mut egui::Ui, label: &str, on: bool) -> bool {
    let text = RichText::new(label)
        .size(12.0)
        .color(if on { BG } else { TEXT });
    ui.add(
        Button::new(text)
            .fill(if on { TEXT } else { BG })
            .stroke(Stroke::new(1.0, if on { TEXT } else { STROKE }))
            .corner_radius(CornerRadius::same(2))
            .min_size(Vec2::new(0.0, 26.0)),
    )
    .clicked()
}

fn text_button(ui: &mut egui::Ui, label: &str) -> bool {
    ui.add(
        Button::new(RichText::new(label).size(12.0).color(TEXT))
            .frame(false)
            .fill(BG),
    )
    .clicked()
}

fn section_toggle(ui: &mut egui::Ui, label: &str, open: bool) -> bool {
    let mark = if open { "–" } else { "+" };
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::click());
    let painter = ui.painter();
    let font = FontId::new(11.0, FontFamily::Proportional);
    let job = LayoutJob::single_section(
        label.to_uppercase(),
        TextFormat {
            font_id: font.clone(),
            extra_letter_spacing: 1.4,
            color: MUTED,
            ..Default::default()
        },
    );
    let galley = painter.layout_job(job);
    painter.galley(
        Pos2::new(rect.left(), rect.center().y - galley.size().y * 0.5),
        galley,
        MUTED,
    );
    painter.text(rect.right_center(), Align2::RIGHT_CENTER, mark, font, MUTED);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}

fn provider_line(ui: &mut egui::Ui, name: &str, tokens: &str, money: &str, sub: bool) -> bool {
    let h = if sub { 18.0 } else { 26.0 };
    let sense = if sub { Sense::hover() } else { Sense::click() };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), sense);
    let painter = ui.painter();
    let font = FontId::new(if sub { 12.0 } else { 14.0 }, FontFamily::Proportional);
    let money_g = painter.layout_no_wrap(money.to_string(), font.clone(), TEXT);
    let tok_g = painter.layout_no_wrap(tokens.to_string(), font.clone(), MUTED);
    let y = rect.center().y;
    let left = rect.left() + if sub { 12.0 } else { 0.0 };
    let name_right = rect.right() - 96.0 - tok_g.size().x - 12.0;
    painter
        .with_clip_rect(Rect::from_min_max(
            rect.min,
            Pos2::new(name_right, rect.bottom()),
        ))
        .text(Pos2::new(left, y), Align2::LEFT_CENTER, name, font, TEXT);
    painter.galley(
        Pos2::new(rect.right() - money_g.size().x, y - money_g.size().y * 0.5),
        money_g,
        TEXT,
    );
    painter.galley(
        Pos2::new(
            rect.right() - 92.0 - tok_g.size().x,
            y - tok_g.size().y * 0.5,
        ),
        tok_g,
        MUTED,
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}

fn model_line(ui: &mut egui::Ui, name: &str, tokens: &str, money: &str) {
    provider_line(ui, name, tokens, money, true);
}

fn note_line(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).size(11.0).color(MUTED)).wrap());
    ui.add_space(1.0);
}

fn meter(ui: &mut egui::Ui, label: &str, value: f64, max: f64, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 18.0), Sense::hover());
    let painter = ui.painter();
    let font = FontId::new(11.0, FontFamily::Proportional);
    painter.text(
        rect.left_center(),
        Align2::LEFT_CENTER,
        label,
        font.clone(),
        MUTED,
    );
    let money = if value <= 0.0 && label == "API" {
        "—".to_string()
    } else {
        fmt_money(value)
    };
    let galley = painter.layout_no_wrap(money, font, TEXT);
    let money_x = rect.right() - galley.size().x;
    painter.galley(
        Pos2::new(money_x, rect.center().y - galley.size().y * 0.5),
        galley,
        TEXT,
    );
    let bar = Rect::from_min_max(
        Pos2::new(rect.left() + 36.0, rect.center().y - 1.0),
        Pos2::new(money_x - 12.0, rect.center().y + 1.0),
    );
    if bar.width() > 2.0 {
        painter.rect_filled(bar, CornerRadius::ZERO, LINE);
        let frac = if max > 0.0 {
            (value / max).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        if frac > 0.0 {
            painter.rect_filled(
                Rect::from_min_max(
                    bar.min,
                    Pos2::new(bar.left() + bar.width() * frac, bar.bottom()),
                ),
                CornerRadius::ZERO,
                color,
            );
        }
    }
}

fn hairline(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, LINE);
}

pub fn fmt_tokens_short(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{} Md", one_decimal(n as f64 / 1_000_000_000.0))
    } else if n >= 1_000_000 {
        format!("{} M", one_decimal(n as f64 / 1_000_000.0))
    } else if n >= 1_000 {
        format!("{} k", one_decimal(n as f64 / 1_000.0))
    } else {
        n.to_string()
    }
}

pub fn fmt_tokens_full(n: u64) -> String {
    group_int(&n.to_string())
}

pub fn fmt_money(usd: f64) -> String {
    if !usd.is_finite() {
        return "—".to_string();
    }
    let neg = usd < 0.0;
    let text = format!("{:.2}", usd.abs());
    let (int, frac) = text.split_once('.').unwrap_or((text.as_str(), "00"));
    let body = format!("{},{frac} $", group_int(int));
    if neg { format!("−{body}") } else { body }
}

fn fmt_ratio(ratio: f64) -> String {
    if ratio >= 10.0 {
        format!("{:.0}", ratio).replace('.', ",")
    } else {
        format!("{ratio:.1}").replace('.', ",")
    }
}

fn one_decimal(v: f64) -> String {
    format!("{v:.1}").replace('.', ",")
}

fn group_int(int: &str) -> String {
    let chars: Vec<char> = int.chars().collect();
    let mut out = String::new();
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i).is_multiple_of(3) {
            out.push('\u{202f}');
        }
        out.push(*c);
    }
    out
}

pub fn tooltip_line(period: Period, tokens: u64, api_usd: f64) -> String {
    let tokens = if tokens == 0 {
        "—".to_string()
    } else {
        fmt_tokens_short(tokens)
    };
    let money = if api_usd <= 0.0 {
        "—".to_string()
    } else {
        fmt_money(api_usd)
    };
    format!("Watt · {} · {tokens} · {money}", period.label())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_and_tokens() {
        assert_eq!(fmt_money(1234.5), "1\u{202f}234,50 $");
        assert_eq!(fmt_money(0.0), "0,00 $");
        assert_eq!(fmt_tokens_short(1_240_000), "1,2 M");
        assert_eq!(fmt_tokens_short(12_400), "12,4 k");
        assert_eq!(fmt_tokens_full(5186044), "5\u{202f}186\u{202f}044");
    }
}
