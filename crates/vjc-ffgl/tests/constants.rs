use vjc_ffgl::abi::*;

#[test]
fn function_codes_match_the_ffgl2_table() {
    assert_eq!(FF_GETINFO, 0);
    assert_eq!(FF_DEINITIALISE, 2);
    assert_eq!(FF_GETNUMPARAMETERS, 4);
    assert_eq!(FF_GETPARAMETERNAME, 5);
    assert_eq!(FF_GETPARAMETERDEFAULT, 6);
    assert_eq!(FF_SETPARAMETER, 8);
    assert_eq!(FF_GETPARAMETER, 9);
    assert_eq!(FF_GETPARAMETERTYPE, 15);
    assert_eq!(FF_PROCESS_OPENGL, 17);
    assert_eq!(FF_INSTANTIATE_GL, 18);
    assert_eq!(FF_DEINSTANTIATE_GL, 19);
    assert_eq!(FF_SET_TIME, 20);
    assert_eq!(FF_RESIZE, 23);
    assert_eq!(FF_INITIALISE_V2, 34);
    assert_eq!(FF_FAIL, 0xFFFF_FFFF);
    assert_eq!(FF_PROCESS_CPU_EXT, 0x7F00);
}
