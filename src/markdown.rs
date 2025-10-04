// Markdown progessing

use eframe::egui::{self, TextStyle};
use eframe::egui::{Align, Layout, Ui, vec2};
use pulldown_cmark::{Event, Options, Parser};

pub fn show(ui: &mut Ui, text: &String) {

    let initial_size = vec2(ui.available_width(), ui.spacing().interact_size.y);
    let layout = Layout::left_to_right(Align::BOTTOM).with_main_wrap(true);

    let mut options = Options::empty();
    options.insert(Options::ENABLE_WIKILINKS | Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(text, options);

    egui::ScrollArea::vertical()
        .id_salt("markdown")
        .max_width(f32::INFINITY)
        .show(ui, |ui| {
            ui.allocate_ui_with_layout(initial_size, layout, |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let row_height = ui.text_style_height(&TextStyle::Body);
                ui.set_row_height(row_height);
                for event in parser {
                    match event {
                        Event::Text(ref text) => {
                            ui.strong(format!("{:}", text));
                            ui.end_row();
                        }
                        _ => {}
                    }
                }
            });
        });
}

// egui::ScrollArea::vertical()
//     .id_salt("markdown")
//     .max_width(f32::INFINITY)
//     .show(ui, |ui| {
//         for event in parser {
//             match event {
//                 Event::Text(ref text) => {
//                     ui.label(format!("{:}", text));
//                 }
//                 _ => {}
//             }
//         }
//     });

// for event in parser {
//     match event {
//         Event::Text(ref text) => {
//             ui.label(format!("{:}", text));
//         }
//         _ => {}
//     }
// }
