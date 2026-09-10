#include <R.h>
#include <Rinternals.h>
#include <Rversion.h>
#include <R_ext/Rdynload.h>

static SEXP scalar_utf8(const char *value) {
    return Rf_mkCharCE(value, CE_UTF8);
}

SEXP hrm_altrep_info(SEXP x) {
    SEXP out = PROTECT(Rf_allocVector(STRSXP, 3));
    if (!ALTREP(x)) {
        SET_STRING_ELT(out, 0, scalar_utf8("0"));
        SET_STRING_ELT(out, 1, scalar_utf8(""));
        SET_STRING_ELT(out, 2, scalar_utf8(""));
        UNPROTECT(1);
        return out;
    }

    SET_STRING_ELT(out, 0, scalar_utf8("1"));
#if R_VERSION >= R_Version(4, 6, 0)
    SET_STRING_ELT(out, 1, Rf_asChar(R_altrep_class_name(x)));
    SET_STRING_ELT(out, 2, Rf_asChar(R_altrep_class_package(x)));
#else
    /* R < 4.6 has no public API for the ALTREP class/package identity.
       Report it as unknown so the R inspector rejects it conservatively. */
    SET_STRING_ELT(out, 1, scalar_utf8("unknown"));
    SET_STRING_ELT(out, 2, scalar_utf8("unknown"));
#endif
    UNPROTECT(1);
    return out;
}

static const R_CallMethodDef call_methods[] = {
    {"hrm_altrep_info", (DL_FUNC) &hrm_altrep_info, 1},
    {NULL, NULL, 0}
};

void R_init_hrm_inspect(DllInfo *dll) {
    R_registerRoutines(dll, NULL, call_methods, NULL, NULL);
    R_useDynamicSymbols(dll, FALSE);
    R_forceSymbols(dll, FALSE);
}
