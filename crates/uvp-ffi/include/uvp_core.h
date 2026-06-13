#ifndef UVP_CORE_H
#define UVP_CORE_H

#ifdef __cplusplus
extern "C" {
#endif

char* uvp_compile_json(const char* request_json);
char* uvp_parse_hook_json(const char* request_json);
char* uvp_eval_hook_json(const char* request_json);
char* uvp_replay_json(const char* request_json);
void uvp_free(char* ptr);
const char* uvp_core_version(void);

#ifdef __cplusplus
}
#endif

#endif
