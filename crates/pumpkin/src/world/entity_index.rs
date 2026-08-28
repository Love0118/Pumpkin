use std::sync::{Arc, RwLock, Weak};

use rustc_hash::FxHashMap;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EntityIndexInsertError {
    DuplicateId(i32),
    DuplicateUuid(Uuid),
}

impl std::fmt::Display for EntityIndexInsertError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(formatter, "duplicate entity id {id}"),
            Self::DuplicateUuid(uuid) => write!(formatter, "duplicate entity UUID {uuid}"),
        }
    }
}

#[derive(Default)]
struct EntityIndexMaps<T: ?Sized> {
    by_id: FxHashMap<i32, Weak<T>>,
    by_uuid: FxHashMap<Uuid, Weak<T>>,
}

/// Atomically owns the two identity views used to resolve a live world entity.
///
/// Weak references keep the world entity list as the lifetime owner. Both maps
/// are changed under one lock so an observer cannot see a half-registered entity.
pub(super) struct EntityIndex<T: ?Sized> {
    maps: RwLock<EntityIndexMaps<T>>,
}

impl<T: ?Sized> Default for EntityIndex<T> {
    fn default() -> Self {
        Self {
            maps: RwLock::new(EntityIndexMaps {
                by_id: FxHashMap::default(),
                by_uuid: FxHashMap::default(),
            }),
        }
    }
}

impl<T: ?Sized> EntityIndex<T> {
    pub(super) fn insert(
        &self,
        id: i32,
        uuid: Uuid,
        entity: &Arc<T>,
    ) -> Result<(), EntityIndexInsertError> {
        let mut maps = self
            .maps
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if maps.by_id.get(&id).and_then(Weak::upgrade).is_some() {
            return Err(EntityIndexInsertError::DuplicateId(id));
        }
        maps.by_id.remove(&id);

        if maps.by_uuid.get(&uuid).and_then(Weak::upgrade).is_some() {
            return Err(EntityIndexInsertError::DuplicateUuid(uuid));
        }
        maps.by_uuid.remove(&uuid);

        let entity = Arc::downgrade(entity);
        maps.by_id.insert(id, entity.clone());
        maps.by_uuid.insert(uuid, entity);
        Ok(())
    }

    pub(super) fn remove(&self, id: i32, uuid: Uuid) -> bool {
        let mut maps = self
            .maps
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let is_same_entity = maps
            .by_id
            .get(&id)
            .zip(maps.by_uuid.get(&uuid))
            .is_some_and(|(by_id, by_uuid)| Weak::ptr_eq(by_id, by_uuid));
        if !is_same_entity {
            return false;
        }

        maps.by_id.remove(&id);
        maps.by_uuid.remove(&uuid);
        true
    }

    pub(super) fn get_by_id(&self, id: i32) -> Option<Arc<T>> {
        self.maps
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .by_id
            .get(&id)
            .and_then(Weak::upgrade)
    }

    pub(super) fn get_by_uuid(&self, uuid: Uuid) -> Option<Arc<T>> {
        self.maps
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .by_uuid
            .get(&uuid)
            .and_then(Weak::upgrade)
    }
}

#[cfg(test)]
mod tests {
    use super::{EntityIndex, EntityIndexInsertError};
    use std::sync::Arc;
    use uuid::Uuid;

    #[test]
    fn insert_publishes_both_identity_views() {
        let index = EntityIndex::default();
        let entity = Arc::new(String::from("entity"));
        let uuid = Uuid::new_v4();

        index.insert(7, uuid, &entity).unwrap();

        assert!(Arc::ptr_eq(&index.get_by_id(7).unwrap(), &entity));
        assert!(Arc::ptr_eq(&index.get_by_uuid(uuid).unwrap(), &entity));
    }

    #[test]
    fn duplicate_id_or_uuid_does_not_publish_half_an_entry() {
        let index = EntityIndex::default();
        let first = Arc::new(String::from("first"));
        let second = Arc::new(String::from("second"));
        let first_uuid = Uuid::new_v4();
        let second_uuid = Uuid::new_v4();
        index.insert(7, first_uuid, &first).unwrap();

        assert_eq!(
            index.insert(7, second_uuid, &second),
            Err(EntityIndexInsertError::DuplicateId(7))
        );
        assert!(index.get_by_uuid(second_uuid).is_none());

        assert_eq!(
            index.insert(8, first_uuid, &second),
            Err(EntityIndexInsertError::DuplicateUuid(first_uuid))
        );
        assert!(index.get_by_id(8).is_none());
    }

    #[test]
    fn remove_requires_matching_id_and_uuid() {
        let index = EntityIndex::default();
        let entity = Arc::new(String::from("entity"));
        let uuid = Uuid::new_v4();
        index.insert(7, uuid, &entity).unwrap();

        assert!(!index.remove(7, Uuid::new_v4()));
        assert!(index.get_by_id(7).is_some());
        assert!(index.remove(7, uuid));
        assert!(index.get_by_id(7).is_none());
        assert!(index.get_by_uuid(uuid).is_none());
    }

    #[test]
    fn dead_weak_entries_can_be_reused() {
        let index = EntityIndex::default();
        let uuid = Uuid::new_v4();
        let entity = Arc::new(String::from("old"));
        index.insert(7, uuid, &entity).unwrap();
        drop(entity);

        let replacement = Arc::new(String::from("new"));
        index.insert(7, uuid, &replacement).unwrap();

        assert!(Arc::ptr_eq(&index.get_by_id(7).unwrap(), &replacement));
    }
}
