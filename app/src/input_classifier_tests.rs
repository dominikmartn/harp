#[test]
#[ignore = "requires the canonical upstream LFS pointer and release classifier features"]
fn test_upstream_pointer_uses_heuristic_fallback() {
    assert_eq!(
        (
            cfg!(feature = "nld_classifier_v3"),
            cfg!(feature = "nld_heuristic_v2"),
            cfg!(feature = "nld_classifier_v1"),
            cfg!(feature = "nld_classifier_v2"),
            cfg!(feature = "nld_heuristic_v1"),
        ),
        (true, true, false, false, false),
        "run with nld_classifier_v3,nld_heuristic_v2 and no older classifier features"
    );

    #[cfg(feature = "nld_classifier_v3")]
    {
        use input_classifier::test_utils::CompletionContext;
        use input_classifier::{
            Context, InputClassificationResult, InputClassifierDecisionSource, InputType,
            OnnxClassifier, OnnxModel,
        };
        use warp_completer::util::parse_current_commands_and_tokens;
        use warpui::{App, SingletonEntity as _};

        use super::InputClassifierModel;

        let model_bytes =
            include_bytes!("../../crates/input_classifier/models/onnx/bert_tiny_v3.onnx");
        assert_eq!(
            model_bytes.as_slice(),
            concat!(
                "version https://git-lfs.github.com/spec/v1\n",
                "oid sha256:d649a9cf72bb8c188cea7f290290f12e3a3ac4031469cd5144f17a500b46b166\n",
                "size 17582121\n",
            )
            .as_bytes(),
            "this test must exercise the canonical upstream pointer, not a hydrated model"
        );
        assert!(
            OnnxClassifier::new(OnnxModel::BertTinyV3).is_err(),
            "the real ONNX constructor must reject the upstream pointer"
        );

        App::test((), |mut app| async move {
            app.update(|ctx| {
                ctx.add_singleton_model(InputClassifierModel::new);
            });
            let classifier = app.read(|ctx| InputClassifierModel::as_ref(ctx).classifier());
            crate::features::mark_initialized();
            let completion_context = CompletionContext::new();

            for (input, current_input_type, expected_input_type, expected_source) in [
                (
                    "cargo --version",
                    InputType::AI,
                    InputType::Shell,
                    InputClassifierDecisionSource::ShellHeuristic,
                ),
                (
                    "What went wrong?",
                    InputType::Shell,
                    InputType::AI,
                    InputClassifierDecisionSource::InputClassifierFallbackHeuristic,
                ),
            ] {
                let parsed =
                    parse_current_commands_and_tokens(input.to_owned(), &completion_context).await;
                let context = Context {
                    current_input_type,
                    is_agent_follow_up: false,
                };
                let classification = classifier
                    .classify_input(parsed.clone(), &context)
                    .await
                    .expect("the app's fallback classifier must classify without ONNX inference");
                assert_eq!(
                    classification.to_input_type(),
                    expected_input_type,
                    "{input}"
                );
                assert_eq!(
                    classification.source,
                    InputClassifierDecisionSource::InputClassifierFallbackHeuristic,
                    "{input} must be classified by the actual heuristic fallback"
                );
                assert_eq!(
                    classifier.detect_input_type(parsed, &context).await,
                    InputClassificationResult::new(expected_input_type, expected_source),
                    "{input} must switch away from the current input type"
                );
            }
        });
    }
}
