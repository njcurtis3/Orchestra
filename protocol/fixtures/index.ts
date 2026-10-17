// The fixture case files, listed by hand because a bundler cannot import a directory.
// The desktop test fails if a fixtures/*.json is missing from this list.
import constants from "./constants.json";
import c0 from "./c2h.call_answer_permission.json";
import c1 from "./c2h.call_answer_permission_message.json";
import c2 from "./c2h.call_answer_question.json";
import c3 from "./c2h.call_answer_reply.json";
import c4 from "./c2h.call_cancel_task.json";
import c5 from "./c2h.call_create_task.json";
import c6 from "./c2h.call_discard_task.json";
import c7 from "./c2h.call_get_task_diff.json";
import c8 from "./c2h.call_get_transcript.json";
import c9 from "./c2h.call_land_task.json";
import c10 from "./c2h.call_list_inbox.json";
import c11 from "./c2h.call_list_projects.json";
import c12 from "./c2h.call_list_sessions.json";
import c13 from "./c2h.call_list_tasks.json";
import c14 from "./c2h.call_retry_cleanup.json";
import c15 from "./c2h.call_start_session.json";
import c16 from "./c2h.call_start_session_settings.json";
import c17 from "./c2h.call_stop_session.json";
import c18 from "./c2h.call_storage_status.json";
import c19 from "./c2h.err_args_field_missing.json";
import c20 from "./c2h.err_args_field_type.json";
import c21 from "./c2h.err_args_given_to_no_arg_method.json";
import c22 from "./c2h.err_args_missing.json";
import c23 from "./c2h.err_args_unknown_field.json";
import c24 from "./c2h.err_bad_answer_kind.json";
import c25 from "./c2h.err_bad_decision.json";
import c26 from "./c2h.err_bad_json.json";
import c27 from "./c2h.err_call_unknown_field.json";
import c28 from "./c2h.err_empty_id.json";
import c29 from "./c2h.err_hello_unknown_field.json";
import c30 from "./c2h.err_id_too_long.json";
import c31 from "./c2h.err_missing_type.json";
import c32 from "./c2h.err_missing_v.json";
import c33 from "./c2h.err_not_object.json";
import c34 from "./c2h.err_pick_project.json";
import c35 from "./c2h.err_ping_unknown_field.json";
import c36 from "./c2h.err_unknown_method.json";
import c37 from "./c2h.err_unknown_model.json";
import c38 from "./c2h.err_unknown_type.json";
import c39 from "./c2h.err_unsupported_version.json";
import c40 from "./c2h.err_unsupported_version_v0.json";
import c41 from "./c2h.err_v_string.json";
import c42 from "./c2h.hello.json";
import c43 from "./c2h.ok_id_max.json";
import c44 from "./c2h.ping.json";
import c45 from "./h2c.err_bad_json.json";
import c46 from "./h2c.err_event_bad_payload.json";
import c47 from "./h2c.err_missing_v.json";
import c48 from "./h2c.err_result_both.json";
import c49 from "./h2c.err_result_neither.json";
import c50 from "./h2c.err_seq_negative.json";
import c51 from "./h2c.err_snapshot_missing_field.json";
import c52 from "./h2c.err_unknown_type.json";
import c53 from "./h2c.err_unsupported_version.json";
import c54 from "./h2c.error.json";
import c55 from "./h2c.error_supported.json";
import c56 from "./h2c.event_inbox_updated.json";
import c57 from "./h2c.event_session_updated.json";
import c58 from "./h2c.event_task_updated.json";
import c59 from "./h2c.event_transcript_appended.json";
import c60 from "./h2c.pong.json";
import c61 from "./h2c.result_error.json";
import c62 from "./h2c.result_ok.json";
import c63 from "./h2c.result_ok_null.json";
import c64 from "./h2c.snapshot.json";
import c65 from "./h2c.snapshot_empty.json";
import c66 from "./h2c.tolerant_error_code.json";
import c67 from "./h2c.tolerant_inbox_kind.json";
import c68 from "./h2c.tolerant_pong_extra.json";
import c69 from "./h2c.tolerant_result_error_code.json";
import c70 from "./h2c.tolerant_session.json";
import c71 from "./h2c.tolerant_task.json";
import c72 from "./h2c.tolerant_unknown_event.json";
import c73 from "./h2c.tolerant_unknown_event_other.json";
import c74 from "./h2c.tolerant_welcome.json";
import c75 from "./h2c.welcome.json";
import c76 from "./hs_c2h.err_bad_json.json";
import c77 from "./hs_c2h.err_missing_v.json";
import c78 from "./hs_c2h.err_unknown_field.json";
import c79 from "./hs_c2h.err_unsupported_version.json";
import c80 from "./hs_c2h.err_wrong_proto.json";
import c81 from "./hs_c2h.request.json";
import c82 from "./hs_h2c.accept.json";
import c83 from "./hs_h2c.err_missing_supported.json";
import c84 from "./hs_h2c.err_unsupported_version.json";
import c85 from "./hs_h2c.err_wrong_proto.json";
import c86 from "./hs_h2c.error_unsupported_version.json";
import c87 from "./h2c.tolerant_transcript_unknown_kind.json";
import c88 from "./h2c.err_transcript_kind_not_string.json";
import c89 from "./c2h.err_v_first_unknown_type.json";
import c90 from "./c2h.err_v_first_unknown_field.json";
import c91 from "./h2c.err_v_first_unknown_type.json";
import c92 from "./hs_c2h.err_v_first_wrong_proto.json";
import c93 from "./hs_h2c.err_v_first_wrong_proto.json";
import c94 from "./c2h.err_id_nonascii_too_long.json";
import c95 from "./c2h.ok_id_nonascii_max.json";
import c96 from "./c2h.ok_id_emoji_max.json";
import c97 from "./c2h.err_id_emoji_too_long.json";
import c98 from "./c2h.call_answer_question_proto_key.json";

export type FixtureCase = {
  name: string;
  dir: "c2h" | "h2c" | "hs_c2h" | "hs_h2c";
  wire?: unknown;
  raw?: string;
  expect: "ok" | { error: string };
  client_only?: boolean;
  decoded_wire?: unknown; // what the decoder re-encodes to when it drops unknown fields
};

export { constants };

export const FIXTURES = [
  c0,
  c1,
  c2,
  c3,
  c4,
  c5,
  c6,
  c7,
  c8,
  c9,
  c10,
  c11,
  c12,
  c13,
  c14,
  c15,
  c16,
  c17,
  c18,
  c19,
  c20,
  c21,
  c22,
  c23,
  c24,
  c25,
  c26,
  c27,
  c28,
  c29,
  c30,
  c31,
  c32,
  c33,
  c34,
  c35,
  c36,
  c37,
  c38,
  c39,
  c40,
  c41,
  c42,
  c43,
  c44,
  c45,
  c46,
  c47,
  c48,
  c49,
  c50,
  c51,
  c52,
  c53,
  c54,
  c55,
  c56,
  c57,
  c58,
  c59,
  c60,
  c61,
  c62,
  c63,
  c64,
  c65,
  c66,
  c67,
  c68,
  c69,
  c70,
  c71,
  c72,
  c73,
  c74,
  c75,
  c76,
  c77,
  c78,
  c79,
  c80,
  c81,
  c82,
  c83,
  c84,
  c85,
  c86,
  c87,
  c88,
  c89,
  c90,
  c91,
  c92,
  c93,
  c94,
  c95,
  c96,
  c97,
  c98,
] as unknown as FixtureCase[];
