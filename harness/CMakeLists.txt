cmake_minimum_required(VERSION 3.20)
project(tog_harness LANGUAGES C)

# Upstream sources exported at the pinned commits by `just harness-build`.
set(TOG_UPSTREAM_DIR "" CACHE PATH "Directory holding the exported upstream repositories")
foreach(repo imuse landru)
    if(NOT EXISTS "${TOG_UPSTREAM_DIR}/${repo}/CMakeLists.txt")
        message(FATAL_ERROR "${repo} is not exported to ${TOG_UPSTREAM_DIR}; build through `just harness-build`")
    endif()
endforeach()

# The reference programs are added with their milestone: imuse_trace (M2), landru_trace (M3). See README.md.
