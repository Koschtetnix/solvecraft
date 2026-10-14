//! Interactive Extrude confirmation runs on a scratch session, off the window thread.
use crate::SolveApp;
#[cfg(not(target_arch = "wasm32"))]
use crate::dialogs::Dialog;
#[cfg(not(target_arch = "wasm32"))]
use serde_json::Value;
use solidcraft_engine::{Session, doc::Document};
use std::sync::{
    Arc,
    mpsc::{Receiver, TryRecvError},
};

struct Running {
    base: Arc<Document>,
    revision: u64,
    cancelled: bool,
    receiver: Receiver<Result<Session, String>>,
}

#[derive(Default)]
pub(crate) struct ExtrudeRun {
    running: Option<Running>,
}

impl ExtrudeRun {
    pub(crate) fn busy(&self) -> bool {
        self.running.as_ref().is_some_and(|job| !job.cancelled)
    }
    pub(crate) fn cancel(&mut self) {
        if let Some(job) = self.running.as_mut() {
            job.cancelled = true;
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn begin(app: &mut SolveApp, dialog: &Dialog, commands: Vec<(String, Value)>, ctx: &egui::Context) -> Result<(), String> {
    if app.extrude_run.running.is_some() {
        return Err("the previous extrusion is still finishing".into());
    }
    let mut scratch = app.session.scratch();
    let editing = dialog.editing;
    let base = app.session.doc.clone();
    let revision = app.session.revision;
    let repaint = ctx.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("extrude-apply".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Some((_, marker)) = editing {
                    let params = marker.map(|position| serde_json::json!({"position":position})).unwrap_or_else(|| serde_json::json!({}));
                    scratch.execute("timeline.roll_to", &params).map_err(|error| error.to_string())?;
                    scratch.undo.clear();
                    scratch.redo.clear();
                }
                for (command, params) in commands {
                    scratch.execute(&command, &params).map_err(|error| error.to_string())?;
                    scratch.echo(format!("{} done", crate::command_label(&command)));
                }
                // Acceptance and the next frame must not trigger cold display meshing.
                for body in &scratch.world_state().bodies {
                    let _ = body.mesh();
                }
                Ok(scratch)
            }))
            .unwrap_or_else(|_| Err("the extrusion failed; the design is unchanged".into()));
            let _ = sender.send(result);
            repaint.request_repaint();
        })
        .map_err(|error| format!("could not start extrusion: {error}"))?;
    app.extrude_run.running = Some(Running { base, revision, cancelled: false, receiver });
    ctx.request_repaint();
    Ok(())
}

pub(crate) fn tick(app: &mut SolveApp, ctx: &egui::Context) {
    let result = app.extrude_run.running.as_ref().and_then(|job| match job.receiver.try_recv() {
        Ok(result) => Some(result),
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Disconnected) => Some(Err("the extrusion stopped; the design is unchanged".into())),
    });
    if let Some(result) = result
        && let Some(job) = app.extrude_run.running.take()
    {
        if job.cancelled {
            return;
        }
        if job.revision != app.session.revision || !Arc::ptr_eq(&job.base, &app.session.doc) {
            app.set_status("the design changed while computing; the extrusion was discarded", true);
            return;
        }
        match result {
            Ok(session) => {
                app.session.accept_computed(session);
                app.dialog = None;
                app.preview.clear();
                app.tool = None;
                app.big_operation = true;
                app.ui.tab = "SOLID".into();
            }
            Err(error) => {
                if let Some(dialog) = app.dialog.as_mut() {
                    dialog.error = Some(error.clone());
                }
                app.set_status(crate::friendly_error("solid.extrude", &error), true);
            }
        }
    }
    if app.extrude_run.running.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::Harness;
    use serde_json::json;

    fn pending(h: &mut Harness) -> std::sync::mpsc::Sender<Result<Session, String>> {
        let (sender, receiver) = std::sync::mpsc::channel();
        h.app.extrude_run.running = Some(Running { base: h.app.session.doc.clone(), revision: h.app.session.revision, cancelled: false, receiver });
        sender
    }

    #[test]
    fn pending_extrusion_keeps_frames_and_read_only_control_responsive() {
        let mut h = Harness::new();
        h.app.run("solid.box", json!({"length":10,"width":10,"height":10})).unwrap();
        h.app.dialog = crate::dialogs::Dialog::for_command(&h.app, "solid.extrude");
        let old = h.app.session.doc.clone();
        let sender = pending(&mut h);
        for _ in 0..3 {
            h.frame();
        }
        let (request, _) = crate::ControlRequest::new("ui.inspect", json!({}));
        let crate::control::Outcome::Done(response) = crate::control::handle(&mut h.app, &h.ctx, &request) else { panic!("inspect") };
        assert_eq!(response["result"]["extrude_busy"], true);
        assert!(h.app.run("edit.undo", json!({})).is_err());
        assert!(Arc::ptr_eq(&old, &h.app.session.doc));
        sender.send(Err("invalid extrusion".into())).unwrap();
        h.frame();
        assert!(!h.app.extrude_run.busy());
        assert!(Arc::ptr_eq(&old, &h.app.session.doc));
        assert_eq!(h.app.dialog.as_ref().unwrap().error.as_deref(), Some("invalid extrusion"));
    }

    #[test]
    fn cancelled_or_obsolete_results_never_replace_the_design() {
        for cancelled in [true, false] {
            let mut h = Harness::new();
            let sender = pending(&mut h);
            let mut computed = h.app.session.scratch();
            computed.execute("solid.box", &json!({"length":10,"width":10,"height":10})).unwrap();
            if cancelled {
                h.app.extrude_run.cancel();
            } else {
                h.app.session.revision += 1;
            }
            sender.send(Ok(computed)).unwrap();
            h.frame();
            assert!(h.app.session.doc.features.is_empty());
            assert!(h.app.session.undo.is_empty());
        }
    }

    #[test]
    fn accepted_result_preserves_path_saved_state_and_prior_undo() {
        let mut h = Harness::new();
        h.app.run("solid.box", json!({"length":10,"width":10,"height":10})).unwrap();
        h.app.session.path = Some("original.solvecraft".into());
        h.app.preview.active = true;
        let preview_slot = h.app.preview.slot.clone();
        let undo = h.app.session.undo.len();
        let sender = pending(&mut h);
        let mut computed = h.app.session.scratch();
        computed.execute("solid.box", &json!({"length":2,"width":2,"height":2})).unwrap();
        let _ = computed.world_state().bodies.iter().map(|body| body.mesh()).collect::<Vec<_>>();
        sender.send(Ok(computed)).unwrap();
        h.frame();
        assert_eq!(h.app.session.path.as_deref(), Some("original.solvecraft"));
        assert!(Arc::ptr_eq(&preview_slot, &h.app.preview.slot));
        assert!(!h.app.preview.active);
        assert_eq!(h.app.session.undo.len(), undo + 1);
        h.app.run("edit.undo", json!({})).unwrap();
        assert_eq!(h.app.session.model.state().bodies.len(), 1);
        h.app.run("edit.undo", json!({})).unwrap();
        assert!(!h.app.session.is_dirty(), "the original saved empty design is still the saved state");
    }
}
