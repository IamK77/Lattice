use super::*;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Record {
    source: String,
    call: String,
    completion: Option<String>,
}

impl Item for Record {
    fn lookup_key(&self) -> Option<&str> {
        Some(&self.call)
    }

    fn same_identity(&self, other: &Self) -> bool {
        self.source == other.source && self.call == other.call
    }
}

fn record(index: usize) -> Record {
    Record {
        source: format!("source-{index}"),
        call: format!("call-{index}"),
        completion: None,
    }
}

#[test]
fn dirty_tail_is_batched_and_bloom_false_positives_still_compare_full_identity() {
    let temp = tempfile::tempdir().unwrap();
    let mut pages = Pages::open(Some(temp.path().into()), Vec::new(), 0).unwrap();
    for index in 0..PAGE_RECORDS {
        pages.push(record(index)).unwrap();
    }
    assert_eq!(
        fs::read_dir(temp.path()).unwrap().count(),
        0,
        "do not publish one full page per new card"
    );
    let slots = pages.directory().unwrap();
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    let absent = (0..4096)
        .map(|index| format!("absent-{index}"))
        .find(|call| {
            slots[0]
                .keys
                .iter()
                .zip(key_mask(call))
                .all(|(bits, required)| bits & required == required)
        })
        .expect("fixture must exercise a real bloom false positive");
    let reader: Pages<Record> = Pages::open(Some(temp.path().into()), slots, PAGE_RECORDS).unwrap();
    assert_eq!(reader.find_last(&absent).unwrap(), None);
    assert_eq!(reader.read_count(), 1);
    assert_eq!(reader.find_last("call-90").unwrap(), Some(90));
}

#[test]
fn growing_old_recipes_split_without_changing_card_ordinals_or_old_checkpoints() {
    let temp = tempfile::tempdir().unwrap();
    let mut pages = Pages::open(Some(temp.path().into()), Vec::new(), 0).unwrap();
    for index in 0..20 {
        pages.push(record(index)).unwrap();
    }
    let prior = pages.directory().unwrap();
    for index in [4, 12] {
        let mut changed = pages.get(index).unwrap();
        changed.completion = Some("large-identity".repeat(PAGE_BYTES / 8));
        pages.replace(index, changed.clone()).unwrap();
        assert_eq!(pages.get(index).unwrap(), changed);
    }
    let slots = pages.directory().unwrap();
    assert!(slots.len() >= 5);
    assert!(slots
        .iter()
        .all(|slot| slot.bytes <= PAGE_BYTES || slot.count == 1));
    let reader: Pages<Record> = Pages::open(Some(temp.path().into()), slots, 20).unwrap();
    for index in 0..20 {
        assert_eq!(reader.get(index).unwrap().source, format!("source-{index}"));
        assert_eq!(
            reader.find_last(&format!("call-{index}")).unwrap(),
            Some(index)
        );
    }
    let prior: Pages<Record> = Pages::open(Some(temp.path().into()), prior, 20).unwrap();
    assert_eq!(prior.get(4).unwrap(), record(4));
    assert_eq!(prior.get(12).unwrap(), record(12));
}

#[test]
fn a_changed_recipe_page_is_an_error_not_an_empty_page_or_missing_command() {
    let temp = tempfile::tempdir().unwrap();
    let mut pages = Pages::open(Some(temp.path().into()), Vec::new(), 0).unwrap();
    pages.push(record(0)).unwrap();
    let slots = pages.directory().unwrap();
    let path = page_path(temp.path(), &slots[0].digest);
    let mut bytes = fs::read(&path).unwrap();
    let offset = bytes
        .windows(8)
        .position(|part| part == b"source-0")
        .unwrap();
    bytes[offset + 7] = b'9';
    fs::write(path, bytes).unwrap();
    let reader: Pages<Record> = Pages::open(Some(temp.path().into()), slots, 1).unwrap();
    assert!(reader.get(0).unwrap_err().to_string().contains("checksum"));
    assert!(reader
        .find_last("call-0")
        .unwrap_err()
        .to_string()
        .contains("checksum"));
}
