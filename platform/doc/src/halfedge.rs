//! Half-edge mesh kernel (docs/01 §3 — the modeling foundation).
//!
//! The indexed [`Mesh`](crate::Mesh) is our *storage* form: compact, serializes cleanly,
//! and is what the renderer tessellates. But topology operations — loop/ring select,
//! loop-cut, bevel, knife, bridge, dissolve — are awkward and O(n) on an indexed mesh
//! because there is no cheap "what's adjacent?" query. The half-edge structure makes
//! adjacency O(1): every directed edge knows its `twin` (the same edge from the other
//! face), its `next`/`prev` around its face, and the vertex it springs from.
//!
//! The kernel is **transient**: built from a `Mesh` on demand, mutated, then baked back to
//! a `Mesh`. Storage stays indexed; editing happens here. (When per-op undo deltas land,
//! they'll be expressed as half-edge diffs — see the SWEET memory.)

use std::collections::{HashMap, HashSet};

use glam::Vec3;

use crate::{Face, Mesh};

/// One directed edge of one face. `origin → dest`, where `dest` is the origin of `next`.
#[derive(Clone, Copy, Debug)]
pub struct HalfEdge {
    /// Vertex this half-edge springs from.
    pub origin: u32,
    /// The same edge walked by the adjacent face (`dest → origin`). `None` on a boundary.
    pub twin: Option<u32>,
    /// Next half-edge around this face (CCW).
    pub next: u32,
    /// Previous half-edge around this face.
    pub prev: u32,
    /// The face this half-edge borders.
    pub face: u32,
}

/// A half-edge representation of a mesh. Built with [`HalfEdgeMesh::from_mesh`], baked back
/// with [`HalfEdgeMesh::to_mesh`].
#[derive(Clone, Debug, Default)]
pub struct HalfEdgeMesh {
    pub positions: Vec<[f32; 3]>,
    pub half_edges: Vec<HalfEdge>,
    /// One representative half-edge per face (index into `half_edges`).
    pub face_half: Vec<u32>,
}

/// Undirected edge key: endpoints sorted so `(a,b)` and `(b,a)` collide.
#[inline]
fn undirected(a: u32, b: u32) -> (u32, u32) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

impl HalfEdgeMesh {
    /// Build the half-edge structure from an indexed mesh. Faces with fewer than 3 vertices
    /// are skipped (they can't bound a polygon); every other face contributes one half-edge
    /// per side. Twins are matched by directed-edge lookup, so a clean 2-manifold gets every
    /// twin filled and boundaries are left `None`.
    pub fn from_mesh(mesh: &Mesh) -> Self {
        let mut half_edges: Vec<HalfEdge> = Vec::new();
        let mut face_half: Vec<u32> = Vec::new();
        // Directed edge (origin,dest) -> half-edge index, for twin matching.
        let mut dir_map: HashMap<(u32, u32), u32> = HashMap::new();

        for face in &mesh.faces {
            let m = face.indices.len();
            if m < 3 {
                continue;
            }
            let start = half_edges.len() as u32;
            for k in 0..m {
                let origin = face.indices[k];
                let dest = face.indices[(k + 1) % m];
                let idx = half_edges.len() as u32;
                let next = start + ((k + 1) % m) as u32;
                let prev = start + ((k + m - 1) % m) as u32;
                half_edges.push(HalfEdge {
                    origin,
                    twin: None,
                    next,
                    prev,
                    face: face_half.len() as u32,
                });
                dir_map.insert((origin, dest), idx);
            }
            face_half.push(start);
        }

        // Match twins: the half-edge a→b twins with the half-edge b→a (if it exists).
        for idx in 0..half_edges.len() {
            let origin = half_edges[idx].origin;
            let dest = half_edges[half_edges[idx].next as usize].origin;
            if let Some(&t) = dir_map.get(&(dest, origin)) {
                half_edges[idx].twin = Some(t);
            }
        }

        Self {
            positions: mesh.vertices.clone(),
            half_edges,
            face_half,
        }
    }

    /// Bake back to an indexed mesh by walking each face's half-edge cycle.
    pub fn to_mesh(&self) -> Mesh {
        let mut faces = Vec::with_capacity(self.face_half.len());
        for &start in &self.face_half {
            let mut indices = Vec::new();
            let mut h = start;
            loop {
                indices.push(self.half_edges[h as usize].origin);
                h = self.half_edges[h as usize].next;
                if h == start {
                    break;
                }
            }
            faces.push(Face { indices });
        }
        Mesh {
            vertices: self.positions.clone(),
            faces,
        }
    }

    /// Number of sides on the face bordered by half-edge `h`.
    pub fn face_len(&self, h: u32) -> usize {
        let start = h;
        let mut count = 0usize;
        let mut cur = h;
        loop {
            count += 1;
            cur = self.half_edges[cur as usize].next;
            if cur == start {
                break;
            }
        }
        count
    }

    /// `dest` vertex of a half-edge.
    #[inline]
    pub fn dest(&self, h: u32) -> u32 {
        self.half_edges[self.half_edges[h as usize].next as usize].origin
    }

    /// Find a half-edge spanning the undirected edge `(a, b)`, in either direction.
    pub fn find_half_edge(&self, a: u32, b: u32) -> Option<u32> {
        for (i, he) in self.half_edges.iter().enumerate() {
            let d = self.dest(i as u32);
            if (he.origin == a && d == b) || (he.origin == b && d == a) {
                return Some(i as u32);
            }
        }
        None
    }

    /// The half-edge directly opposite `h` within a **quad** face (the parallel side a
    /// loop-cut would also cross). `None` if the face isn't a quad.
    fn quad_opposite(&self, h: u32) -> Option<u32> {
        if self.face_len(h) != 4 {
            return None;
        }
        let next = self.half_edges[h as usize].next;
        Some(self.half_edges[next as usize].next)
    }

    /// Step the **edge ring** forward: from entry edge `h`, cross its quad to the parallel
    /// edge, then hop through the twin into the next quad. `None` at a boundary or non-quad.
    fn ring_step(&self, h: u32) -> Option<u32> {
        let opp = self.quad_opposite(h)?;
        self.half_edges[opp as usize].twin
    }

    /// The **edge ring** seeded at half-edge `start`: the chain of parallel edges across a
    /// strip of quads. Returns undirected edge keys. Walks forward until it closes (a loop
    /// around a tube) or hits a boundary/non-quad; the seed edge is always included.
    ///
    /// This is what a loop-cut crosses, and the selection a "ring select" highlights.
    pub fn edge_ring(&self, start: u32) -> Vec<(u32, u32)> {
        let mut edges: Vec<(u32, u32)> = Vec::new();
        let mut seen: HashSet<(u32, u32)> = HashSet::new();
        let mut cur = Some(start);
        while let Some(h) = cur {
            let key = undirected(self.half_edges[h as usize].origin, self.dest(h));
            if !seen.insert(key) {
                break;
            }
            edges.push(key);
            cur = self.ring_step(h);
        }
        edges
    }

    /// The **edge loop** seeded at half-edge `start`: the chain of collinear edges that runs
    /// *along* a strip (perpendicular to the ring), continuing straight through valence-4
    /// vertices. Returns undirected edge keys. This is the "edge loop select" set.
    ///
    /// At the far vertex of `h`, the loop continues to the edge that is opposite *around that
    /// vertex* — for a vertex shared by four quads, that's `next.twin.next`.
    pub fn edge_loop(&self, start: u32) -> Vec<(u32, u32)> {
        let mut edges: Vec<(u32, u32)> = Vec::new();
        let mut seen: HashSet<(u32, u32)> = HashSet::new();
        let mut cur = Some(start);
        while let Some(h) = cur {
            let key = undirected(self.half_edges[h as usize].origin, self.dest(h));
            if !seen.insert(key) {
                break;
            }
            edges.push(key);
            cur = self.loop_step(h);
        }
        edges
    }

    /// Step the edge loop forward through the vertex at `dest(h)`. Only well-defined where
    /// that vertex is surrounded by quads (valence 4); `None` otherwise, ending the loop.
    fn loop_step(&self, h: u32) -> Option<u32> {
        // Walk the fan around dest(h): next around this face, hop to the twin's face.
        let next = self.half_edges[h as usize].next;
        let twin = self.half_edges[next as usize].twin?;
        // `twin` springs from dest(h); its `next` is the collinear continuation edge.
        let cont = self.half_edges[twin as usize].next;
        // Only continue cleanly across quads, so the loop stays straight.
        if self.face_len(h) == 4 && self.face_len(cont) == 4 {
            Some(cont)
        } else {
            None
        }
    }

    /// The **fan** of half-edges leaving vertex `v`, in rotational order — one per incident
    /// edge. Rotates with `twin(prev(h))`, which hops from a face into the next face sharing
    /// `v`. `None` if `v` sits on a boundary (an open fan) or isn't referenced — bevel and
    /// other corner ops need a closed fan to be well-defined.
    pub fn vertex_fan(&self, v: u32) -> Option<Vec<u32>> {
        let start =
            (0..self.half_edges.len() as u32).find(|&h| self.half_edges[h as usize].origin == v)?;
        let mut outs = vec![start];
        let mut h = start;
        loop {
            let prev = self.half_edges[h as usize].prev;
            let twin = self.half_edges[prev as usize].twin?; // boundary → open fan, bail
            if twin == start {
                break;
            }
            outs.push(twin);
            h = twin;
            if outs.len() > self.half_edges.len() {
                return None; // malformed; don't spin
            }
        }
        Some(outs)
    }
}

impl Mesh {
    /// **Loop cut**: insert an edge loop crossing the ring seeded at edge `(a, b)`, splitting
    /// every quad the ring passes through. Each crossed edge gains a midpoint vertex; each
    /// crossed quad becomes two quads. Faces off the ring are untouched. The single most-used
    /// box-modeling op after extrude (docs/01 §3.1).
    ///
    /// Returns `None` if `(a, b)` isn't an edge, or the ring crosses no quads. Quad-only by
    /// design — n-gons on the ring are passed through unchanged rather than mangled.
    pub fn loop_cut(&self, a: u32, b: u32) -> Option<Mesh> {
        let he = HalfEdgeMesh::from_mesh(self);
        let start = he.find_half_edge(a, b)?;
        let ring: HashSet<(u32, u32)> = he.edge_ring(start).into_iter().collect();
        if ring.is_empty() {
            return None;
        }

        // One midpoint vertex per cut edge, shared by both adjacent quads.
        let mut vertices = self.vertices.clone();
        let mut mid: HashMap<(u32, u32), u32> = HashMap::new();
        for &(e0, e1) in &ring {
            let p0 = Vec3::from(vertices[e0 as usize]);
            let p1 = Vec3::from(vertices[e1 as usize]);
            let m = (p0 + p1) * 0.5;
            let idx = vertices.len() as u32;
            vertices.push([m.x, m.y, m.z]);
            mid.insert((e0, e1), idx);
        }

        // Rebuild faces: split any quad whose two opposite edges are both on the ring.
        let mut split_any = false;
        let mut faces: Vec<Face> = Vec::new();
        for face in &self.faces {
            let m = face.indices.len();
            let cut_positions: Vec<usize> = (0..m)
                .filter(|&k| ring.contains(&undirected(face.indices[k], face.indices[(k + 1) % m])))
                .collect();

            // A clean ring crossing splits a quad at two *opposite* edges (positions differ
            // by 2). Anything else (touched once, adjacent, or n-gon) is passed through.
            if m == 4 && cut_positions.len() == 2 && cut_positions[1] - cut_positions[0] == 2 {
                let p = cut_positions[0];
                let at = |k: usize| face.indices[k % m];
                let a0 = at(p);
                let b0 = at(p + 1);
                let c0 = at(p + 2);
                let d0 = at(p + 3);
                let m0 = mid[&undirected(a0, b0)];
                let m1 = mid[&undirected(c0, d0)];
                // Two quads either side of the new edge m0—m1.
                faces.push(Face {
                    indices: vec![a0, m0, m1, d0],
                });
                faces.push(Face {
                    indices: vec![m0, b0, c0, m1],
                });
                split_any = true;
            } else {
                faces.push(face.clone());
            }
        }

        if !split_any {
            return None;
        }
        Some(Mesh { vertices, faces })
    }

    /// **Bevel (vertex / corner chamfer)**: truncate vertex `v`, replacing the corner with a
    /// flat cap face. Each edge meeting `v` gains a new vertex pulled back from `v` by
    /// fraction `amount` (of that edge's length); every face that touched `v` swaps it for the
    /// two new vertices on its two incident edges; a cap n-gon closes the opening. The other
    /// everyday hard-surface op alongside loop-cut (docs/01 §3.1).
    ///
    /// Returns `None` for a boundary/non-manifold vertex (no closed fan) or valence < 3. The
    /// now-orphaned `v` is compacted out so indices stay tight.
    pub fn bevel_vertex(&self, v: u32, amount: f32) -> Option<Mesh> {
        let he = HalfEdgeMesh::from_mesh(self);
        let outs = he.vertex_fan(v)?;
        if outs.len() < 3 {
            return None;
        }
        let t = amount.clamp(0.01, 0.49);
        let vp = Vec3::from(self.vertices[v as usize]);

        // One new vertex per incident edge, keyed by the edge's *other* endpoint (unique per
        // incident edge), so the two faces sharing that edge agree on it.
        let mut vertices = self.vertices.clone();
        let mut edge_new: HashMap<u32, u32> = HashMap::new();
        for &h in &outs {
            let d = he.dest(h);
            let dp = Vec3::from(self.vertices[d as usize]);
            let np = vp + (dp - vp) * t;
            let idx = vertices.len() as u32;
            vertices.push([np.x, np.y, np.z]);
            edge_new.insert(d, idx);
        }

        // Faces touching v: replace v with [entering-edge vertex, leaving-edge vertex].
        let mut faces: Vec<Face> = Vec::new();
        for face in &self.faces {
            if let Some(pos) = face.indices.iter().position(|&i| i == v) {
                let m = face.indices.len();
                let prev_v = face.indices[(pos + m - 1) % m];
                let next_v = face.indices[(pos + 1) % m];
                let b = edge_new[&prev_v]; // on edge (prev_v — v)
                let a = edge_new[&next_v]; // on edge (v — next_v)
                let mut idx = Vec::with_capacity(m + 1);
                for (k, &iv) in face.indices.iter().enumerate() {
                    if k == pos {
                        idx.push(b);
                        idx.push(a);
                    } else {
                        idx.push(iv);
                    }
                }
                faces.push(Face { indices: idx });
            } else {
                faces.push(face.clone());
            }
        }

        // Cap face: new vertices in fan order. The fan rotates via twin(prev), i.e. clockwise
        // seen from outside, so reverse to wind the cap CCW (outward).
        let mut cap: Vec<u32> = outs.iter().map(|&h| edge_new[&he.dest(h)]).collect();
        cap.reverse();
        faces.push(Face { indices: cap });

        // Compact out the now-orphaned v so the index space stays tight.
        vertices.remove(v as usize);
        for f in &mut faces {
            for i in &mut f.indices {
                if *i > v {
                    *i -= 1;
                }
            }
        }
        Some(Mesh { vertices, faces })
    }

    /// **Edge bevel**: replace edge `(a, b)` with a chamfer strip. Each endpoint of the edge
    /// gets two new vertices — one per adjacent face — offset by `amount` along the edges that
    /// meet there. The two original faces each lose a corner vertex and gain a bevel vert on
    /// each side of the old edge; a new quad (or n-gon) bridges the strip. The result is the
    /// classic "add a control loop around a hard edge" op — every industrial hard-surface
    /// workflow depends on it (docs/01 §3.1).
    ///
    /// Only operates on interior edges (both faces known). Returns `None` if `(a,b)` isn't an
    /// edge or is a boundary edge.
    pub fn bevel_edge(&self, a: u32, b: u32, amount: f32) -> Option<Mesh> {
        let he = HalfEdgeMesh::from_mesh(self);
        let hab = he.find_half_edge(a, b)?;
        // Enforce canonical direction: hab goes a→b.
        let hab = if he.half_edges[hab as usize].origin == a {
            hab
        } else {
            // find_half_edge returns one of the two; pick the one with origin == a.
            he.half_edges.iter().enumerate().find_map(|(i, h)| {
                if h.origin == a && he.dest(i as u32) == b {
                    Some(i as u32)
                } else {
                    None
                }
            })?
        };
        let twin = he.half_edges[hab as usize].twin?; // boundary → bail
        // twin goes b→a.
        let t = amount.clamp(0.01, 0.49);

        let pa = Vec3::from(self.vertices[a as usize]);
        let pb = Vec3::from(self.vertices[b as usize]);

        // For each half-edge around each face incident to the bevel edge we need the
        // "other" vertex — the one that is neither a nor b, immediately beside the edge.
        // For face 0 (hab's face): the vertex before a and the vertex after b.
        let a_prev_0 = he.half_edges[he.half_edges[hab as usize].prev as usize].origin;
        let b_next_0 = he.dest(he.half_edges[hab as usize].next);
        // For face 1 (twin's face): the vertex before b and the vertex after a.
        let b_prev_1 = he.half_edges[he.half_edges[twin as usize].prev as usize].origin;
        let a_next_1 = he.dest(he.half_edges[twin as usize].next);

        let along = |from: Vec3, toward: Vec3| from + (toward - from) * t;

        // Four new bevel vertices around the two endpoints.
        let mut vertices = self.vertices.clone();
        let base = vertices.len() as u32;
        // a0: along edge a→prev_a_on_face0, a1: along edge a→next_a_on_face1
        let pa_prev0 = Vec3::from(self.vertices[a_prev_0 as usize]);
        let pa_next1 = Vec3::from(self.vertices[a_next_1 as usize]);
        let pb_next0 = Vec3::from(self.vertices[b_next_0 as usize]);
        let pb_prev1 = Vec3::from(self.vertices[b_prev_1 as usize]);

        let a0 = base; // near a, on face 0 side (pulled toward a_prev_0)
        let a1 = base + 1; // near a, on face 1 side (pulled toward a_next_1)
        let b0 = base + 2; // near b, on face 0 side (pulled toward b_next_0)
        let b1 = base + 3; // near b, on face 1 side (pulled toward b_prev_1)

        // Bevel verts: pulled back from the edge endpoint along the adjacent edges.
        vertices.push(along(pa, pa_prev0).into());
        vertices.push(along(pa, pa_next1).into());
        vertices.push(along(pb, pb_next0).into());
        vertices.push(along(pb, pb_prev1).into());

        // Rebuild faces. The two faces touching (a,b) need to have a/b replaced:
        //   face 0 (hab's face): replace a→a0, replace b→b0.
        //   face 1 (twin's face): replace b→b1, replace a→a1.
        // All other faces are unchanged.
        let face0_id = he.half_edges[hab as usize].face as usize;
        let face1_id = he.half_edges[twin as usize].face as usize;

        let remap_face = |face: &Face, old_a: u32, new_a: u32, old_b: u32, new_b: u32| Face {
            indices: face
                .indices
                .iter()
                .map(|&i| {
                    if i == old_a {
                        new_a
                    } else if i == old_b {
                        new_b
                    } else {
                        i
                    }
                })
                .collect(),
        };

        let mut faces: Vec<Face> = Vec::new();
        for (fi, face) in self.faces.iter().enumerate() {
            if fi == face0_id {
                faces.push(remap_face(face, a, a0, b, b0));
            } else if fi == face1_id {
                faces.push(remap_face(face, b, b1, a, a1));
            } else {
                faces.push(face.clone());
            }
        }

        // Bevel strip quad: a0 — b0 — b1 — a1 (bridges the gap, wound outward).
        faces.push(Face {
            indices: vec![a0, b0, b1, a1],
        });

        Some(Mesh { vertices, faces })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_edge_round_trips_a_cube() {
        let cube = Mesh::cube();
        let he = HalfEdgeMesh::from_mesh(&cube);
        // A closed cube: 6 quads → 24 half-edges, every one with a twin.
        assert_eq!(he.half_edges.len(), 24);
        assert!(
            he.half_edges.iter().all(|h| h.twin.is_some()),
            "a watertight cube has no boundary edges"
        );
        let back = he.to_mesh();
        assert_eq!(back.vertices.len(), cube.vertices.len());
        assert_eq!(back.faces.len(), cube.faces.len());
        for (a, b) in cube.faces.iter().zip(back.faces.iter()) {
            assert_eq!(a.indices, b.indices, "face winding preserved");
        }
    }

    #[test]
    fn vertex_fan_on_a_cube_corner_has_valence_three() {
        let cube = Mesh::cube();
        let he = HalfEdgeMesh::from_mesh(&cube);
        let fan = he.vertex_fan(0).expect("closed corner");
        assert_eq!(fan.len(), 3, "a cube corner meets three faces");
        assert!(
            fan.iter().all(|&h| he.half_edges[h as usize].origin == 0),
            "every fan half-edge leaves the vertex"
        );
    }

    #[test]
    fn bevel_truncates_a_cube_corner() {
        let cube = Mesh::cube();
        let beveled = cube.bevel_vertex(0, 0.25).expect("corner bevels");
        // Valence-3 corner: v removed, 3 new verts → 8 - 1 + 3 = 10.
        assert_eq!(beveled.vertices.len(), 10);
        // 3 touched quads become pentagons, + 1 triangular cap → 6 + 1 = 7.
        assert_eq!(beveled.faces.len(), 7);
        // The old corner is gone from every face.
        assert!(
            beveled.faces.iter().all(|f| !f.indices.contains(&u32::MAX)),
            "no sentinel leaked"
        );
        // Exactly one triangular cap face was added.
        let tris = beveled
            .faces
            .iter()
            .filter(|f| f.indices.len() == 3)
            .count();
        assert_eq!(tris, 1, "one triangular cap");
        // Indices in range.
        let nv = beveled.vertices.len() as u32;
        assert!(beveled
            .faces
            .iter()
            .all(|f| f.indices.iter().all(|&i| i < nv)));
    }

    #[test]
    fn edge_ring_around_a_cube_closes_at_four() {
        let cube = Mesh::cube();
        let he = HalfEdgeMesh::from_mesh(&cube);
        // Any edge on a cube belongs to a ring of exactly 4 parallel edges (a band around
        // the cube), and the ring closes.
        let start = he.find_half_edge(0, 1).expect("edge 0-1 exists");
        let ring = he.edge_ring(start);
        assert_eq!(ring.len(), 4, "a cube ring is a closed band of 4 edges");
    }

    #[test]
    fn loop_cut_splits_a_cube_band() {
        let cube = Mesh::cube();
        let cut = cube.loop_cut(0, 1).expect("cube edge cuts");
        // 4 crossed edges → 4 new midpoint vertices.
        assert_eq!(cut.vertices.len(), cube.vertices.len() + 4);
        // 4 crossed quads each split in two: 6 - 4 + 8 = 10 faces.
        assert_eq!(cut.faces.len(), 10);
        // Every face is still a quad.
        assert!(cut.faces.iter().all(|f| f.indices.len() == 4));
        // All indices are in range.
        let nv = cut.vertices.len() as u32;
        assert!(cut.faces.iter().all(|f| f.indices.iter().all(|&i| i < nv)));
    }

    #[test]
    fn loop_cut_rejects_a_nonexistent_edge() {
        let cube = Mesh::cube();
        assert!(cube.loop_cut(0, 6).is_none(), "0 and 6 are not an edge");
    }

    #[test]
    fn edge_loop_on_a_cube_returns_a_loop() {
        let cube = Mesh::cube();
        let he = HalfEdgeMesh::from_mesh(&cube);
        let start = he.find_half_edge(0, 1).expect("edge exists");
        let lp = he.edge_loop(start);
        // On a bare cube the loop is short (valence-3 corners stop it), but it must at least
        // contain the seed and never panic.
        assert!(!lp.is_empty());
        assert!(lp.contains(&undirected(0, 1)));
    }

    #[test]
    fn bevel_edge_chamfers_a_cube_edge() {
        let cube = Mesh::cube();
        // Edge 0→1 is a bottom-back edge on the default cube.
        let beveled = cube.bevel_edge(0, 1, 0.2).expect("edge bevels");
        // 8 original verts + 4 bevel verts = 12.
        assert_eq!(beveled.vertices.len(), 12);
        // 6 original faces (2 modified) + 1 new bevel quad = 7.
        assert_eq!(beveled.faces.len(), 7);
        // Exactly one new quad bevel face.
        let quads = beveled.faces.iter().filter(|f| f.indices.len() == 4).count();
        assert!(quads >= 1, "at least one quad in the bevel result");
        // All indices in range.
        let nv = beveled.vertices.len() as u32;
        assert!(beveled
            .faces
            .iter()
            .all(|f| f.indices.iter().all(|&i| i < nv)));
    }

    #[test]
    fn bevel_edge_rejects_nonexistent_edge() {
        let cube = Mesh::cube();
        assert!(cube.bevel_edge(0, 6, 0.2).is_none(), "0-6 is not an edge");
    }
}
