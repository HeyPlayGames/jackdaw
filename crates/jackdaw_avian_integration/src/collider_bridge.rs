//! Builds avian [`Collider`] components from [`AvianCollider`] at runtime.
//!
//! Jackdaw scenes persist [`AvianCollider`] (a `ColliderConstructor` wrapper) rather
//! than the derived [`Collider`] mesh. This module turns that recipe into real
//! collision geometry for brush entities ([`Brush`]) and mesh-backed entities
//! ([`Mesh3d`]) after scene load and when geometry changes.

use avian3d::prelude::*;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use jackdaw_geometry::{compute_brush_geometry_from_planes, is_convex_topology};
use jackdaw_jsn::Brush;

use crate::AvianCollider;

pub struct PhysicsColliderBridgePlugin;

impl Plugin for PhysicsColliderBridgePlugin {
    fn build(&self, app: &mut App) {
        crate::register_avian_types(app);
        app.add_systems(PostUpdate, sync_avian_collider_config)
            .add_observer(remove_collider_when_avian_collider_removed);
    }
}

fn remove_collider_when_avian_collider_removed(
    trigger: On<Remove, AvianCollider>,
    mut commands: Commands,
) {
    let entity = trigger.event_target();
    if let Ok(mut entity_commands) = commands.get_entity(entity) {
        entity_commands.try_remove::<Collider>();
    }
}

fn sync_avian_collider_config(
    mut commands: Commands,
    changed: Query<
        (
            Entity,
            &AvianCollider,
            Option<&Brush>,
            Option<&Mesh3d>,
            Option<&RigidBody>,
        ),
        Or<(
            Added<AvianCollider>,
            Changed<AvianCollider>,
            Added<Brush>,
            Changed<Brush>,
        )>,
    >,
    meshes: Res<Assets<Mesh>>,
) {
    for (entity, config, brush, mesh3d, rigid_body) in &changed {
        let constructor = collider_constructor(config, brush, rigid_body);

        let collider = if constructor.requires_mesh() {
            if let Some(brush) = brush {
                let Some(mesh) = brush_collision_mesh(brush) else {
                    continue;
                };
                Collider::try_from_constructor(constructor.clone(), Some(&mesh))
            } else if let Some(mesh3d) = mesh3d {
                let Some(mesh) = meshes.get(&mesh3d.0) else {
                    continue;
                };
                Collider::try_from_constructor(constructor.clone(), Some(mesh))
            } else {
                continue;
            }
        } else {
            Collider::try_from_constructor(constructor.clone(), None)
        };

        if let Some(collider) = collider {
            commands.entity(entity).insert(collider);
        }
    }
}

fn collider_constructor(
    config: &AvianCollider,
    brush: Option<&Brush>,
    rigid_body: Option<&RigidBody>,
) -> ColliderConstructor {
    let Some(brush) = brush else {
        return config.0.clone();
    };

    if !is_convex_topology(&brush.topology) {
        return ColliderConstructor::TrimeshFromMesh;
    }

    if rigid_body.is_some_and(RigidBody::is_dynamic) {
        return ColliderConstructor::ConvexHullFromMesh;
    }

    return config.0.clone();
}

fn brush_collision_mesh(brush: &Brush) -> Option<Mesh> {
    let (vertices, face_polygons) = if !brush.topology.polygons.is_empty() {
        let vertices: Vec<Vec3> = brush
            .topology
            .vertices
            .iter()
            .map(|vertex| vertex.position)
            .collect();
        let face_polygons: Vec<Vec<usize>> = (0..brush.topology.polygons.len())
            .map(|face_index| {
                brush
                    .topology
                    .face_ring(face_index)
                    .map(|vertex_index| vertex_index as usize)
                    .collect()
            })
            .collect();
        (vertices, face_polygons)
    } else {
        compute_brush_geometry_from_planes(&brush.faces)
    };

    if vertices.is_empty() {
        return None;
    }

    let positions: Vec<[f32; 3]> = vertices.iter().map(|vertex| vertex.to_array()).collect();
    let mut indices: Vec<u32> = Vec::new();
    for polygon in &face_polygons {
        if polygon.len() >= 3 {
            for triangle_index in 1..polygon.len() - 1 {
                indices.push(polygon[0] as u32);
                indices.push(polygon[triangle_index] as u32);
                indices.push(polygon[triangle_index + 1] as u32);
            }
        }
    }
    if indices.is_empty() {
        return None;
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_indices(Indices::U32(indices));
    return Some(mesh);
}
