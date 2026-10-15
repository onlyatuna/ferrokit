// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::types::*;
use ferrokit_core::variant::Variant;
use std::collections::{BTreeMap, HashMap, HashSet};

// =============================================================================
// 1. StringList & StringListExt Tests
// =============================================================================

#[test]
fn test_string_list_operations() {
    let mut list = StringList::from(vec!["banana", "apple", "cherry", "apple", "date"]);

    // Index & Deref
    assert_eq!(list[0], "banana");
    assert_eq!(list.len(), 5);

    // Join
    assert_eq!(list.join(";"), "banana;apple;cherry;apple;date");

    // Contains & IndexOf
    assert!(list.contains("apple"));
    assert!(!list.contains("grape"));
    assert_eq!(list.index_of("apple"), Some(1));
    assert_eq!(list.last_index_of("apple"), Some(3));

    // Case-insensitive search
    assert!(list.contains_case_insensitive("APPLE"));

    // Filter
    let filtered = list.filter("an");
    assert_eq!(filtered.as_vec(), &["banana"]);

    // Deduplication (preserves first occurrence order)
    list.remove_duplicates();
    assert_eq!(list.as_vec(), &["banana", "apple", "cherry", "date"]);

    // Sort
    list.sort();
    assert_eq!(list.as_vec(), &["apple", "banana", "cherry", "date"]);

    // Replace in strings
    list.replace_in_strings("a", "@");
    assert_eq!(list.as_vec(), &["@pple", "b@n@n@", "cherry", "d@te"]);

    // Split
    let parsed = StringList::split("red,green,blue", ",");
    assert_eq!(parsed.as_vec(), &["red", "green", "blue"]);

    let ws = StringList::split_whitespace("  one   two \t three \n");
    assert_eq!(ws.as_vec(), &["one", "two", "three"]);
}

#[test]
fn test_string_list_ext_trait() {
    let native_vec = vec!["one".to_string(), "two".to_string(), "three".to_string()];
    assert_eq!(native_vec.join_qt(" - "), "one - two - three");

    let filtered = native_vec.filter_qt("t");
    assert_eq!(filtered, vec!["two", "three"]);

    let qlist = native_vec.to_string_list();
    assert_eq!(qlist.len(), 3);
}

// =============================================================================
// 2. ByteArray Operations, Hex, and Base64 Tests
// =============================================================================

#[test]
fn test_byte_array_text_and_binary() {
    let mut ba = ByteArray::from("Hello, World!");
    assert_eq!(ba.as_str().unwrap(), "Hello, World!");
    assert_eq!(ba.len(), 13);

    // Starts/Ends with & Contains
    assert!(ba.starts_with(b"Hello"));
    assert!(ba.ends_with(b"World!"));
    assert!(ba.contains(b"lo, W"));
    assert_eq!(ba.index_of(b"World"), Some(7));

    // Replace
    ba.replace(b"World", b"ferrokit");
    assert_eq!(ba.as_str().unwrap(), "Hello, ferrokit!");

    // Trimmed and simplified
    let dirty = ByteArray::from(" \t\r\n alpha \t beta \n gamma \r\n ");
    assert_eq!(dirty.trimmed().as_str().unwrap(), "alpha \t beta \n gamma");
    assert_eq!(dirty.simplified().as_str().unwrap(), "alpha beta gamma");

    // Split
    let csv = ByteArray::from("x:y:z");
    let parts = csv.split(b':');
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0].as_str().unwrap(), "x");
    assert_eq!(parts[1].as_str().unwrap(), "y");
    assert_eq!(parts[2].as_str().unwrap(), "z");
}

#[test]
fn test_byte_array_hex_and_base64() {
    let raw = ByteArray::from(&b"\x00\x01\x0A\xFF\x42"[..]);

    // Hex
    let hex = raw.to_hex();
    assert_eq!(hex, "00010aff42");
    let decoded_hex = ByteArray::from_hex(&hex).unwrap();
    assert_eq!(decoded_hex, raw);

    // Invalid hex
    assert!(ByteArray::from_hex("123").is_err()); // odd length
    assert!(ByteArray::from_hex("zz").is_err()); // invalid char

    // Base64
    let text = ByteArray::from(
        "Man is distinguished, not only by his reason, but by this singular passion",
    );
    let b64 = text.to_base64();
    let decoded_b64 = ByteArray::from_base64(&b64).unwrap();
    assert_eq!(decoded_b64, text);

    // Short strings padding check
    assert_eq!(ByteArray::from("M").to_base64(), "TQ==");
    assert_eq!(ByteArray::from("Ma").to_base64(), "TWE=");
    assert_eq!(ByteArray::from("Man").to_base64(), "TWFu");

    assert_eq!(
        ByteArray::from_base64("TQ==").unwrap().as_str().unwrap(),
        "M"
    );
    assert_eq!(
        ByteArray::from_base64("TWE=").unwrap().as_str().unwrap(),
        "Ma"
    );
    assert_eq!(
        ByteArray::from_base64("TWFu").unwrap().as_str().unwrap(),
        "Man"
    );
}

// =============================================================================
// 3. Queue and Stack Tests
// =============================================================================

#[test]
fn test_queue_and_stack_fifo_lifo() {
    // Queue (FIFO)
    let mut q = Queue::new();
    q.enqueue(10);
    q.enqueue(20);
    q.enqueue(30);

    assert_eq!(q.head(), Some(&10));
    assert_eq!(q.len(), 3);
    assert_eq!(q.dequeue(), Some(10));
    assert_eq!(q.dequeue(), Some(20));
    assert_eq!(q.dequeue(), Some(30));
    assert_eq!(q.dequeue(), None);
    assert!(q.is_empty());

    // Stack (LIFO)
    let mut s = Stack::new();
    s.push(10);
    s.push(20);
    s.push(30);

    assert_eq!(s.top(), Some(&30));
    assert_eq!(s.len(), 3);
    assert_eq!(s.pop(), Some(30));
    assert_eq!(s.pop(), Some(20));
    assert_eq!(s.pop(), Some(10));
    assert_eq!(s.pop(), None);
    assert!(s.is_empty());
}

// =============================================================================
// 4. MultiMap and MultiHash Tests
// =============================================================================

#[test]
fn test_multimap_and_multihash_qt_semantics() {
    // MultiMap (ordered by key)
    let mut m = MultiMap::new();
    m.insert("fruits", "apple");
    m.insert("fruits", "banana");
    m.insert("fruits", "cherry");
    m.insert("vegetables", "carrot");

    // Qt semantic: get() returns the LAST inserted value for that key
    assert_eq!(m.get(&"fruits"), Some(&"cherry"));
    assert_eq!(m.get(&"vegetables"), Some(&"carrot"));

    // get_all returns all values
    assert_eq!(m.get_all(&"fruits"), &["apple", "banana", "cherry"]);

    assert_eq!(m.len(), 4);
    assert_eq!(m.key_count(), 2);
    assert_eq!(m.unique_keys(), vec![&"fruits", &"vegetables"]);

    // Removal
    assert!(m.remove_value(&"fruits", &"banana"));
    assert_eq!(m.get_all(&"fruits"), &["apple", "cherry"]);
    assert!(!m.remove_value(&"fruits", &"nonexistent"));

    let removed = m.remove(&"fruits").unwrap();
    assert_eq!(removed, vec!["apple", "cherry"]);
    assert!(!m.contains_key(&"fruits"));

    // MultiHash (unordered)
    let mut h = MultiHash::new();
    h.insert(1, "one-a");
    h.insert(1, "one-b");
    assert_eq!(h.get(&1), Some(&"one-b"));
    assert_eq!(h.get_all(&1), &["one-a", "one-b"]);
    assert_eq!(h.len(), 2);
}

// =============================================================================
// 5. BitArray Tests
// =============================================================================

#[test]
fn test_bit_array_operations() {
    let mut bits = BitArray::with_size(10, false);
    assert_eq!(bits.size(), 10);
    assert_eq!(bits.count(true), 0);
    assert_eq!(bits.count(false), 10);

    bits.set_bit(0, true);
    bits.set_bit(3, true);
    bits.set_bit(9, true);

    assert!(bits.test_bit(0));
    assert!(!bits.test_bit(1));
    assert!(bits.test_bit(3));
    assert!(bits.test_bit(9));
    assert_eq!(bits.count(true), 3);

    // Toggle
    assert!(!bits.toggle_bit(0)); // was true -> now false
    assert!(!bits.test_bit(0));

    // Fill
    bits.fill(true);
    assert_eq!(bits.count(true), 10);
    assert_eq!(bits.count(false), 0);

    // Resize
    bits.resize(15, false);
    assert_eq!(bits.size(), 15);
    assert_eq!(bits.count(true), 10);
    assert_eq!(bits.count(false), 5);

    bits.resize(5, false);
    assert_eq!(bits.size(), 5);
    assert_eq!(bits.count(true), 5);

    // Bitwise operators
    let mut a = BitArray::with_size(4, false);
    a.set_bit(0, true);
    a.set_bit(1, true); // 1100 (bits 0,1)

    let mut b = BitArray::with_size(4, false);
    b.set_bit(1, true);
    b.set_bit(2, true); // 0110 (bits 1,2)

    let and_res = a.clone() & b.clone();
    assert!(!and_res.test_bit(0));
    assert!(and_res.test_bit(1));
    assert!(!and_res.test_bit(2));

    let or_res = a.clone() | b.clone();
    assert!(or_res.test_bit(0));
    assert!(or_res.test_bit(1));
    assert!(or_res.test_bit(2));
    assert!(!or_res.test_bit(3));

    let not_res = !a;
    assert!(!not_res.test_bit(0));
    assert!(!not_res.test_bit(1));
    assert!(not_res.test_bit(2));
    assert!(not_res.test_bit(3));
}

// =============================================================================
// 6. Variant Data Structures Integration Tests
// =============================================================================

#[test]
fn test_variant_containers_integration() {
    // ByteArray in Variant
    let ba = ByteArray::from("Hello Qt Variant");
    let var: Variant = ba.clone().into();
    assert_eq!(var.type_name(), "QByteArray");

    let retrieved_ba = var.to_byte_array().unwrap();
    assert_eq!(retrieved_ba, ba);

    let try_ba: Result<ByteArray, _> = var.try_into();
    assert_eq!(try_ba.unwrap(), ba);

    // StringList in Variant
    let sl = StringList::from(vec!["red", "green", "blue"]);
    let sl_var: Variant = sl.clone().into();
    assert_eq!(sl_var.type_name(), "QVariantList");

    let retrieved_sl = sl_var.to_string_list().unwrap();
    assert_eq!(retrieved_sl, sl);

    let try_sl: Result<StringList, _> = sl_var.try_into();
    assert_eq!(try_sl.unwrap(), sl);
}

// =============================================================================
// 7. Canonical Type Aliases Compilation & Usability Tests
// =============================================================================

#[test]
fn test_canonical_type_aliases() {
    let _s: QString = "hello".to_string();
    let _sv: QStringView = "slice";
    let _usv: QUtf8StringView = "utf8_slice";
    let _sl: QStringList = StringList::new();
    let _ba: QByteArray = ByteArray::new();
    let _bav: QByteArrayView = b"bytes";
    let _bal: QByteArrayList = vec![ByteArray::new()];

    let _list: QList<i32> = vec![1, 2, 3];
    let _vec: QVector<i32> = vec![4, 5, 6];
    let _queue: QQueue<i32> = Queue::new();
    let _stack: QStack<i32> = Stack::new();

    let mut _hash: QHash<String, i32> = HashMap::new();
    _hash.insert("one".to_string(), 1);

    let mut _map: QMap<String, i32> = BTreeMap::new();
    _map.insert("two".to_string(), 2);

    let mut _set: QSet<i32> = HashSet::new();
    _set.insert(42);

    let _mm: QMultiMap<String, i32> = MultiMap::new();
    let _mh: QMultiHash<String, i32> = MultiHash::new();
    let _bits: QBitArray = BitArray::new();
    let _pair: QPair<i32, String> = (10, "ten".to_string());
    let arr = [10, 20, 30];
    let _span: QSpan<i32> = &arr[..];
    let _varlen: QVarLengthArray<i32> = vec![1, 2, 3];
    let _shared_str: SharedStr = std::sync::Arc::from("shared");

    let _vl: QVariantList = vec![Variant::from(100)];
    let _vm: QVariantMap = BTreeMap::new();
    let _vh: QVariantHash = HashMap::new();
    let _vp: QVariantPair = (Variant::from(1), Variant::from(2));

    assert_eq!(_list.len(), 3);
    assert_eq!(_pair.0, 10);
    assert_eq!(_span[1], 20);
}
