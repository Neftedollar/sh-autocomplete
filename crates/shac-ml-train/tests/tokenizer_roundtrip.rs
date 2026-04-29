use shac_ml_train::tokenizer::{Vocab, SPECIAL_TOKENS};

#[test]
fn special_tokens_have_stable_ids_at_front() {
    let vocab = Vocab::new_with_special_only();
    // First special token is <PAD> per the SPECIAL_TOKENS array
    assert_eq!(vocab.id_of(SPECIAL_TOKENS[0]), Some(0));
    assert_eq!(vocab.token_of(0), Some(SPECIAL_TOKENS[0]));
    // <UNK> sentinel
    assert!(vocab.id_of("<UNK>").is_some());
}

#[test]
fn build_from_corpus_preserves_special_tokens_first() {
    let corpus = vec![
        "git status".to_string(),
        "git add .".to_string(),
        "cargo test".to_string(),
    ];
    let vocab = Vocab::build_from_corpus(&corpus, /*max_size=*/ 50);
    // Special tokens occupy ids 0..N
    for (i, &tok) in SPECIAL_TOKENS.iter().enumerate() {
        assert_eq!(vocab.id_of(tok), Some(i as u32));
    }
    // 'git' should be in the vocab (frequency-ranked)
    assert!(vocab.id_of("git").is_some());
}

#[test]
fn unknown_word_maps_to_unk() {
    let vocab = Vocab::new_with_special_only();
    let id = vocab.encode_word("never-seen-word-12345");
    assert_eq!(id, vocab.id_of("<UNK>").unwrap());
}

#[test]
fn save_load_json_roundtrip() {
    let vocab = Vocab::new_with_special_only();
    let json = vocab.to_json().unwrap();
    let restored = Vocab::from_json(&json).unwrap();
    assert_eq!(vocab.id_of("<UNK>"), restored.id_of("<UNK>"));
    assert_eq!(vocab.size(), restored.size());
}
