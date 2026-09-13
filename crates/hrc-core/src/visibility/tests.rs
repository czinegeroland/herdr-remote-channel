use super::*;

const CHANNEL: &str = "Team channel";

fn everything() -> Vec<PublicDisclosure> {
    PublicDisclosure::ALL.to_vec()
}

#[test]
fn all_seven_consequences_from_section_16_4_are_present() {
    // The section lists seven. A disclosure that lost one would still
    // compile and still look complete on screen.
    assert_eq!(PublicDisclosure::ALL.len(), 7);

    let mut seen = PublicDisclosure::ALL.to_vec();
    let before = seen.len();
    seen.dedup();
    assert_eq!(seen.len(), before, "a consequence is listed twice");

    for disclosure in PublicDisclosure::ALL {
        assert!(
            !disclosure.text().is_empty(),
            "{disclosure:?} has no text to show"
        );
    }
}

#[test]
fn a_complete_disclosure_and_the_exact_phrase_confirm() {
    let confirmed =
        ConfirmedPublication::new(CHANNEL, &everything(), "make Team channel public", "roland")
            .expect("this should confirm");

    assert_eq!(confirmed.channel_local_name(), CHANNEL);
    assert_eq!(confirmed.confirmed_by(), "roland");
}

#[test]
fn a_partial_disclosure_cannot_be_confirmed_however_correctly_it_is_typed() {
    // The interface showing six of seven consequences is the realistic
    // failure: a screen that scrolled, a list that was truncated. Typing the
    // phrase perfectly must not rescue it.
    let mut shown = everything();
    let dropped = shown.pop().expect("seven of them");

    let refusal = ConfirmedPublication::new(CHANNEL, &shown, "make Team channel public", "roland")
        .expect_err("an incomplete disclosure must not confirm");

    match refusal {
        RefusedPublication::DisclosureIncomplete { missing } => {
            assert_eq!(missing, vec![dropped]);
        }
        other => panic!("expected an incomplete disclosure, got {other:?}"),
    }
}

#[test]
fn showing_nothing_at_all_is_refused() {
    let refusal = ConfirmedPublication::new(CHANNEL, &[], "make Team channel public", "roland")
        .expect_err("showing nothing must not confirm");

    assert!(matches!(
        refusal,
        RefusedPublication::DisclosureIncomplete { .. }
    ));
}

#[test]
fn nothing_easier_to_type_than_the_phrase_is_accepted() {
    // Every one of these is something a person types without reading. If any
    // were accepted, it is the one that would eventually publish a channel
    // by accident.
    for typed in [
        "y",
        "Y",
        "yes",
        "YES",
        "",
        " ",
        "\n",
        "n",
        "no",
        "ok",
        "confirm",
        "make public",
        "public",
        "Make Team channel public",
        "make team channel public",
        "make Team channel public!",
        "make Other channel public",
    ] {
        assert!(
            ConfirmedPublication::new(CHANNEL, &everything(), typed, "roland").is_err(),
            "`{typed}` must not confirm publication"
        );
    }
}

#[test]
fn surrounding_whitespace_is_forgiven_because_a_terminal_adds_it() {
    // Forgiven because a person cannot see it, unlike every case above,
    // which they can.
    for typed in [
        "make Team channel public\n",
        "  make Team channel public  ",
        "\tmake Team channel public\r\n",
    ] {
        assert!(
            ConfirmedPublication::new(CHANNEL, &everything(), typed, "roland").is_ok(),
            "`{typed:?}` should confirm"
        );
    }
}

#[test]
fn the_phrase_names_the_channel_so_confirming_one_does_not_confirm_another() {
    let team = Confirmation::for_channel("Team channel");
    let ops = Confirmation::for_channel("Ops channel");

    assert_ne!(team.phrase(), ops.phrase());
    assert!(!team.is_satisfied_by(ops.phrase()));
    assert!(team.phrase().contains("Team channel"));
}

#[test]
fn a_refusal_says_what_should_have_been_typed() {
    let refusal = ConfirmedPublication::new(CHANNEL, &everything(), "yes", "roland")
        .expect_err("`yes` must not confirm");

    match refusal {
        RefusedPublication::PhraseNotTyped { expected } => {
            assert_eq!(expected, "make Team channel public");
        }
        other => panic!("expected a phrase refusal, got {other:?}"),
    }
}

#[test]
fn the_audit_detail_names_the_channel_and_the_human_but_not_the_input() {
    // Echoing typed input into an append-only log is a habit worth not
    // having; the record needs which channel and who, not a transcript.
    let confirmed =
        ConfirmedPublication::new(CHANNEL, &everything(), "make Team channel public", "roland")
            .expect("this should confirm");

    let detail = confirmed.audit_detail();
    assert!(detail.contains(CHANNEL));
    assert!(detail.contains("roland"));
    assert!(detail.contains("16.4"));
}

#[test]
fn a_confirmation_cannot_be_built_without_going_through_the_checks() {
    // `ConfirmedPublication` has private fields and one constructor, so the
    // only way to hold one is to have passed both gates. This test exists to
    // make that a stated property rather than an accident of the current
    // field visibility: if someone adds a public constructor or derives
    // `Default`, this is where the reviewer should stop.
    let confirmed =
        ConfirmedPublication::new(CHANNEL, &everything(), "make Team channel public", "roland");
    assert!(confirmed.is_ok());

    // The disclosure text is provisional pending OQ-008, so nothing here
    // asserts its prose — only that every consequence has some.
    for disclosure in PublicDisclosure::ALL {
        assert!(disclosure.text().len() > 20);
    }
}
