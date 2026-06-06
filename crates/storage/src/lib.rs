//! M13: Web Storage API（localStorage / sessionStorage）。
//!
//! 为 SPA 爬虫提供持久化能力——很多 React/Vue 应用在
//! `localStorage` 里塞 token、用户偏好、缓存数据。
//!
//! 实现细节：
//! - 后端是 `Rc<RefCell<HashMap<String, String>>>`（同 boa bridge 的 Tree 模式）。
//! - localStorage 和 sessionStorage 在 MVP 阶段共享一个 store
//!   （爬虫场景不区分，session 一般也不会跨刷新）。
//! - 持久化（写到磁盘）推迟到产品化阶段。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Web Storage 后端。Rc<RefCell<...>> 让 boa NativeFunction
/// 能从 thread-local slot 里拿到同一个 store。
pub type StorageHandle = Rc<RefCell<HashMap<String, String>>>;

/// 创建一个新的（空的）storage handle。
pub fn new_storage() -> StorageHandle {
    Rc::new(RefCell::new(HashMap::new()))
}

/// `localStorage.getItem(key)` → `Option<String>`。
pub fn storage_get(store: &StorageHandle, key: &str) -> Option<String> {
    store.borrow().get(key).cloned()
}

/// `localStorage.setItem(key, value)`。
pub fn storage_set(store: &StorageHandle, key: &str, value: &str) {
    store
        .borrow_mut()
        .insert(key.to_string(), value.to_string());
}

/// `localStorage.removeItem(key)`。
pub fn storage_remove(store: &StorageHandle, key: &str) -> Option<String> {
    store.borrow_mut().remove(key)
}

/// `localStorage.clear()`。
pub fn storage_clear(store: &StorageHandle) {
    store.borrow_mut().clear();
}

/// `localStorage.length`。
pub fn storage_len(store: &StorageHandle) -> usize {
    store.borrow().len()
}

/// `localStorage.key(index)` → `Option<String>`。
///
/// 顺序按插入序（HashMap 是无序的，但 Web 标准也不保证跨实现
/// 一致性，爬虫场景够用）。
pub fn storage_key(store: &StorageHandle, index: usize) -> Option<String> {
    let borrowed = store.borrow();
    if index >= borrowed.len() {
        return None;
    }
    // 收集 keys 并排序，保证可重现（HashMap 无序）。
    let mut keys: Vec<String> = borrowed.keys().cloned().collect();
    drop(borrowed);
    keys.sort();
    keys.into_iter().nth(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get() {
        let s = new_storage();
        storage_set(&s, "foo", "bar");
        assert_eq!(storage_get(&s, "foo"), Some("bar".to_string()));
    }

    #[test]
    fn get_missing_returns_none() {
        let s = new_storage();
        assert_eq!(storage_get(&s, "missing"), None);
    }

    #[test]
    fn set_overwrites() {
        let s = new_storage();
        storage_set(&s, "k", "v1");
        storage_set(&s, "k", "v2");
        assert_eq!(storage_get(&s, "k"), Some("v2".to_string()));
    }

    #[test]
    fn remove_returns_old_value() {
        let s = new_storage();
        storage_set(&s, "k", "v");
        assert_eq!(storage_remove(&s, "k"), Some("v".to_string()));
        assert_eq!(storage_get(&s, "k"), None);
    }

    #[test]
    fn clear_wipes_all() {
        let s = new_storage();
        storage_set(&s, "a", "1");
        storage_set(&s, "b", "2");
        storage_clear(&s);
        assert_eq!(storage_len(&s), 0);
    }

    #[test]
    fn len_reflects_count() {
        let s = new_storage();
        assert_eq!(storage_len(&s), 0);
        storage_set(&s, "a", "1");
        storage_set(&s, "b", "2");
        assert_eq!(storage_len(&s), 2);
    }

    #[test]
    fn key_by_index_sorted() {
        let s = new_storage();
        storage_set(&s, "z", "1");
        storage_set(&s, "a", "2");
        // keys 排序后是 ["a", "z"]
        assert_eq!(storage_key(&s, 0), Some("a".to_string()));
        assert_eq!(storage_key(&s, 1), Some("z".to_string()));
        assert_eq!(storage_key(&s, 2), None);
    }

    #[test]
    fn handle_clones_share_state() {
        let s1 = new_storage();
        let s2 = Rc::clone(&s1);
        storage_set(&s1, "shared", "yes");
        // 通过 clone 看到同一份数据。
        assert_eq!(storage_get(&s2, "shared"), Some("yes".to_string()));
    }
}
