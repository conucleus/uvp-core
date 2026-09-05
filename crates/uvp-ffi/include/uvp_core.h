#ifndef UVP_CORE_H
#define UVP_CORE_H

#ifdef __cplusplus
extern "C" {
#endif

char* uvp_compile_json(const char* request_json);
char* uvp_parse_hook_json(const char* request_json);
char* uvp_eval_compiled_hook_json(const char* request_json);
char* uvp_replay_json(const char* request_json);
void uvp_free(char* ptr);
const char* uvp_core_version(void);
/* 构建指纹（git-<rev>，build.rs 编译期烧入）；宿主侧比对当前 uvp-core 检出
 * HEAD 以识别陈旧产物。旧产物无此符号，宿主侧须用 dlsym 弱探测。 */
const char* uvp_core_build_fingerprint(void);

#ifdef __cplusplus
}
#endif

#endif
