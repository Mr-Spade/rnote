use ink_stroke_modeler_rs::{
    ModelerInput, ModelerInputEventType, PredictionParams, StrokeModeler, StrokeModelerParams,
};
use p2d::bounding_volume::Aabb;
use piet::RenderContext;
use std::time::Instant;

use crate::penevents::PenEvent;
use crate::penpath::{Element, Segment};
use crate::style::Composer;
use crate::{PenPath, Style};

use super::{Constraints, PenPathBuilderBehaviour, PenPathBuilderCreator, PenPathBuilderProgress};

/// The pen path builder
pub struct PenPathModeledBuilder {
    /// Buffered elements, which are filled up by new pen events and used to build path segments
    pub buffer: Vec<Element>,
    prediction_start: Element,
    /// Holding the current prediction. Is recalculated after the modeler is updated with a new element
    prediction_buffer: Vec<Element>,
    start_time: Instant,
    last_element: Element,
    last_element_time: Instant,
    stroke_modeler: StrokeModeler,
}

impl std::fmt::Debug for PenPathModeledBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModeledPenPathBuilder")
            .field("buffer", &self.buffer)
            .field("prediction_buffer", &self.prediction_buffer)
            .field("start_time", &self.start_time)
            .field("last_element", &self.last_element)
            .field("last_element_time", &self.last_element_time)
            .field("stroke_modeler", &".. no debug impl ..")
            .finish()
    }
}

impl PenPathBuilderCreator for PenPathModeledBuilder {
    fn start(element: Element, now: Instant) -> Self {
        let mut builder = Self {
            buffer: vec![],
            prediction_start: element,
            prediction_buffer: vec![],
            start_time: now,
            last_element: element,
            last_element_time: now,
            stroke_modeler: StrokeModeler::default(),
        };

        builder.restart(element, now);

        builder
    }
}

impl PenPathBuilderBehaviour for PenPathModeledBuilder {
    fn handle_event(
        &mut self,
        event: PenEvent,
        now: Instant,
        _constraints: Constraints,
    ) -> PenPathBuilderProgress {
        /*         log::debug!(
            "event: {:?}; buffer.len(): {}, state: {:?}",
            event,
            self.buffer.len(),
            self.state
        ); */

        match event {
            PenEvent::Down { element, .. } => {
                // kDown is already fed into the modeler when the builder was instantiated (with start())
                self.update_modeler_w_element(element, ModelerInputEventType::kMove, now);

                match self.try_build_segments() {
                    Some(segments) => PenPathBuilderProgress::EmitContinue(segments),
                    None => PenPathBuilderProgress::InProgress,
                }
            }
            PenEvent::Up { element, .. } => {
                self.update_modeler_w_element(element, ModelerInputEventType::kUp, now);

                let segments = self.build_segments_end();

                PenPathBuilderProgress::Finished(segments)
            }
            PenEvent::Proximity { .. } | PenEvent::KeyPressed { .. } | PenEvent::Text { .. } => {
                PenPathBuilderProgress::InProgress
            }
            PenEvent::Cancel => PenPathBuilderProgress::Finished(vec![]),
        }
    }

    fn bounds(&self, style: &Style, _zoom: f64) -> Option<Aabb> {
        PenPath::try_from_elements(
            self.buffer
                .iter()
                .chain(std::iter::once(&self.prediction_start))
                .chain(self.prediction_buffer.iter())
                .copied(),
        )
        .map(|pp| pp.composed_bounds(style))
    }

    fn draw_styled(&self, cx: &mut piet_cairo::CairoRenderContext, style: &Style, _zoom: f64) {
        cx.save().unwrap();

        let pen_path = PenPath::try_from_elements(
            self.buffer
                .iter()
                .chain(std::iter::once(&self.prediction_start))
                .chain(self.prediction_buffer.iter())
                .copied(),
        );

        /*
                // Change prediction stroke color for debugging
                let mut style = style.clone();
                match style {
                    Style::Smooth(ref mut smooth_options) => {
                        smooth_options.stroke_color = Some(crate::Color::RED)
                    }
                    _ => {}
                }
        */

        if let Some(pen_path) = pen_path {
            pen_path.draw_composed(cx, style);
        }

        cx.restore().unwrap();
    }
}

impl PenPathModeledBuilder {
    const MODELER_MIN_OUTPUT_RATE: f64 = 120.0;
    const MODELER_MAX_OUTPUTS_PER_CALL: i32 = 100;

    fn try_build_segments(&mut self) -> Option<Vec<Segment>> {
        if self.buffer.is_empty() {
            return None;
        }

        Some(
            self.buffer
                .drain(..)
                .map(|el| Segment::LineTo { end: el })
                .collect(),
        )
    }

    fn build_segments_end(&mut self) -> Vec<Segment> {
        self.buffer
            .drain(..)
            .map(|el| Segment::LineTo { end: el })
            .collect()
    }

    fn update_modeler_w_element(
        &mut self,
        element: Element,
        event_type: ModelerInputEventType,
        now: Instant,
    ) {
        if self.last_element == element {
            // Can't feed modeler with duplicate elements, or results in `INVALID_ARGUMENT` errors
            return;
        }
        self.last_element = element;

        let n_steps = (now.duration_since(self.last_element_time).as_secs_f64()
            * Self::MODELER_MIN_OUTPUT_RATE)
            .ceil() as i32;

        if n_steps > Self::MODELER_MAX_OUTPUTS_PER_CALL {
            // If the no of outputs the modeler would need to produce exceeds the configured maximum
            // ( because the time delta between the last elements is too large ), it needs to be restarted.
            log::debug!("penpathmodeledbuilder: update_modeler_w_element(): n_steps exceeds configured max outputs per call, restarting modeler");

            self.restart(element, now);
        }
        self.last_element_time = now;

        let modeler_input = ModelerInput::new(
            event_type,
            (element.pos[0] as f32, element.pos[1] as f32),
            now.duration_since(self.start_time).as_secs_f64(),
            element.pressure as f32,
            0.0,
            0.0,
        );

        //log::debug!("{modeler_input}");

        self.buffer.extend(
            self.stroke_modeler
                .update(modeler_input)
                .into_iter()
                .map(|r| {
                    let pos = r.get_pos();
                    let pressure = r.get_pressure();

                    Element::new(na::vector![pos.0 as f64, pos.1 as f64], pressure as f64)
                }),
        );

        // The prediction start is the last buffer element (which will get drained)
        if let Some(last) = self.buffer.last() {
            self.prediction_start = *last;
        }

        // When the stroke is finished it is invalid to predict, and the existing prediction should be cleared.
        if event_type == ModelerInputEventType::kUp {
            self.prediction_buffer.clear();
        } else {
            self.prediction_buffer = self
                .stroke_modeler
                .predict()
                .into_iter()
                .map(|r| {
                    let pos = r.get_pos();
                    let pressure = r.get_pressure();

                    Element::new(na::vector![pos.0 as f64, pos.1 as f64], pressure as f64)
                })
                .collect::<Vec<Element>>();
        }
    }

    fn restart(&mut self, element: Element, now: Instant) {
        let params = StrokeModelerParams {
            sampling_min_output_rate: Self::MODELER_MIN_OUTPUT_RATE,
            sampling_max_outputs_per_call: Self::MODELER_MAX_OUTPUTS_PER_CALL,
            prediction_params: PredictionParams::StrokeEnd,
            ..StrokeModelerParams::suggested()
        };

        self.buffer.clear();
        self.prediction_buffer.clear();
        self.start_time = now;
        self.last_element_time = now;
        self.last_element = element;
        self.stroke_modeler.reset_w_params(params);

        self.buffer.extend(
            self.stroke_modeler
                .update(ModelerInput::new(
                    ModelerInputEventType::kDown,
                    (element.pos[0] as f32, element.pos[1] as f32),
                    0.0,
                    element.pressure as f32,
                    0.0,
                    0.0,
                ))
                .into_iter()
                .map(|r| {
                    let pos = r.get_pos();
                    let pressure = r.get_pressure();

                    Element::new(na::vector![pos.0 as f64, pos.1 as f64], pressure as f64)
                }),
        );
    }
}
