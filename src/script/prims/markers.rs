//! Markers: positions that move with edits.

use super::*;

pub(in crate::script) fn register(reg: &mut Registrar, ctx: &EngineShared) {
    let EngineShared { markers, .. } = ctx;
    reg.section(Section::Markers);
    // --- Markers (positions that move with edits) -------------------------
    {
        // `(make-marker pos)` → a marker id (allocated synchronously).
        let m = markers.clone();
        reg.register_fn("make-marker", move |pos: isize| -> usize {
            m.lock().unwrap().alloc(scheme_pos(pos)) as usize
        });
    }
    {
        // `(marker-position id)` → its current byte position, or -1.
        let m = markers.clone();
        reg.register_fn("marker-position", move |id: u64| -> isize {
            m.lock()
                .unwrap()
                .positions
                .get(&id)
                .map_or(-1, |p| *p as isize)
        });
    }
    {
        let m = markers.clone();
        reg.register_fn("set-marker", move |id: u64, pos: isize| {
            m.lock().unwrap().positions.insert(id, scheme_pos(pos));
        });
    }
    {
        let m = markers.clone();
        reg.register_fn("delete-marker", move |id: u64| {
            m.lock().unwrap().positions.remove(&id);
        });
    }
}
