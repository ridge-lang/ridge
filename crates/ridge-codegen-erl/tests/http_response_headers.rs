//! Validated response metadata and real loopback redirect following.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_codegen_erl::{erlc, output_layout, runtime};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn validated_response_headers_and_local_redirect() {
    let directory = tempfile::tempdir().unwrap();
    output_layout::ensure_out_dirs(directory.path()).unwrap();
    runtime::install_runtime(directory.path()).unwrap();
    let compiler = erlc::probe(None).expect("beam-runtime requires OTP");
    runtime::compile_runtime(&compiler.path, directory.path()).unwrap();
    let eval = r#"
        Validate = fun ridge_rt:http_validate_response_headers/1,
        {ok, ok} = Validate([]),
        {ok, ok} = Validate([{<<"Location">>, <<"/target">>}]),
        {ok, ok} = Validate([{<<"Set-Cookie">>, <<"a=b">>}, {<<"set-cookie">>, <<"c=d">>}]),
        Bad = [
            [{<<"Bad Name">>, <<"value">>}],
            [{<<"">>, <<"value">>}],
            [{binary:copy(<<"a">>, 257), <<"value">>}],
            [{<<"X-Test">>, <<"a\r\nInjected: yes">>}],
            [{<<"X-Test">>, <<0>>}],
            [{<<"X-Test">>, <<127>>}],
            [{<<"X-Test">>, <<255>>}],
            [{<<"Location">>, <<"/a">>}, {<<"LOCATION">>, <<"/b">>}],
            [{<<"Content-Type">>, <<"a">>}, {<<"content-type">>, <<"b">>}],
            [{<<"X-Test">>, binary:copy(<<"a">>, 8193)}],
            lists:duplicate(101, {<<"X-Test">>, <<"a">>}),
            lists:duplicate(9, {<<"X-Test">>, binary:copy(<<"a">>, 8192)}),
            [{<<"X-Test">>, wrong}], wrong
        ] ++ [[{Name, <<"value">>}] || Name <- [<<"Content-Length">>, <<"transfer-encoding">>,
            <<"CONNECTION">>, <<"Trailer">>, <<"Upgrade">>, <<"Keep-Alive">>,
            <<"Proxy-Connection">>, <<"Content-Security-Policy">>, <<"Strict-Transport-Security">>]],
        lists:foreach(fun(H) ->
            {error, #{code := <<"invalid_http_headers">>}} = Validate(H),
            R = ridge_rt:http_build_response(#{status => 302, body => <<"unsafe">>, headers => H}),
            {0, _} = binary:match(R, <<"HTTP/1.0 500 ">>),
            nomatch = binary:match(R, <<"Injected: yes">>),
            nomatch = binary:match(R, <<"unsafe">>)
        end, Bad),
        Plain = #{status => 200, body => <<"ok">>},
        Same = ridge_rt:http_build_response(Plain),
        Same = ridge_rt:http_build_response(Plain#{headers => []}),
        Json = ridge_rt:http_build_response(Plain#{headers => [{<<"content-type">>, <<"application/json">>}]}),
        nomatch = binary:match(Json, <<"Content-Type: text/plain">>),
        {_, _} = binary:match(Json, <<"content-type: application/json\r\n">>),
        Cookies = ridge_rt:http_build_response(Plain#{headers => [{<<"Set-Cookie">>, <<"a=b">>}, {<<"Set-Cookie">>, <<"c=d">>}]}),
        2 = length(binary:matches(Cookies, <<"Set-Cookie: ">>)),
        1 = length(binary:matches(Cookies, <<"Content-Security-Policy: ">>)),
        1 = length(binary:matches(Cookies, <<"Strict-Transport-Security: ">>)),
        Handler = fun(#{path := Path}) ->
            case Path of
                <<"/target">> -> #{status => 200, body => <<"arrived">>};
                _ -> #{status => 302, body => <<"redirect">>, headers => [{<<"Location">>, <<"/target">>}]}
            end
        end,
        spawn(fun() -> ridge_rt:http_listen(0, Handler) end),
        Wait = fun F() -> case ridge_rt:http_port() of
            P when is_integer(P) -> P; _ -> timer:sleep(10), F() end end,
        Port = Wait(),
        {ok, _} = application:ensure_all_started(inets),
        Url = "http://127.0.0.1:" ++ integer_to_list(Port) ++ "/redirect",
        {ok, {{_, 302, "Found"}, Fields, _}} = httpc:request(get, {Url, []}, [{autoredirect, false}], []),
        "/target" = proplists:get_value("location", Fields),
        "default-src 'self'" = proplists:get_value("content-security-policy", Fields),
        "max-age=31536000" = proplists:get_value("strict-transport-security", Fields),
        {ok, {{_, 200, _}, _, "arrived"}} = httpc:request(get, {Url, []}, [], []),
        io:put_chars("headers and redirect ok\n"), halt().
    "#;
    let mut child = Command::new(which::which("erl").expect("beam-runtime requires erl"))
        .args(["-noshell", "-pa"])
        .arg(output_layout::beam_dir(directory.path()))
        .args(["-eval", eval])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(30) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("response header test exceeded deadline");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "headers and redirect ok"
    );
}
