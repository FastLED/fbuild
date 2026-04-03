#!/bin/bash
set -e

# Ensure we're in dev mode for testing
export FBUILD_DEV_MODE=1

# Color codes for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Helper functions
log_step() {
    echo -e "\n${BLUE}===================================================================${NC}"
    echo -e "${BLUE}Step $1: $2${NC}"
    echo -e "${BLUE}===================================================================${NC}\n"
}

log_success() {
    echo -e "${GREEN}✓ $1${NC}"
}

log_error() {
    echo -e "${RED}✗ $1${NC}"
}

log_info() {
    echo -e "${YELLOW}ℹ $1${NC}"
}

# Track test results
TESTS_PASSED=0
TESTS_FAILED=0

assert_success() {
    if [ $? -eq 0 ]; then
        log_success "$1"
        ((TESTS_PASSED++))
        return 0
    else
        log_error "$1"
        ((TESTS_FAILED++))
        return 0  # Don't exit script, just record failure
    fi
}

assert_exit_code() {
    local expected=$1
    local actual=$2
    local message=$3

    if [ "$actual" -eq "$expected" ]; then
        log_success "$message (exit code $actual)"
        ((TESTS_PASSED++))
        return 0
    else
        log_error "$message (expected $expected, got $actual)"
        ((TESTS_FAILED++))
        return 0  # Don't exit script, just record failure
    fi
}

# Start test
echo -e "${GREEN}"
echo "╔═══════════════════════════════════════════════════════════════════╗"
echo "║                 fbuild Purge Command Test Suite                  ║"
echo "║                        UNO Platform                               ║"
echo "╚═══════════════════════════════════════════════════════════════════╝"
echo -e "${NC}"

log_info "Test environment: FBUILD_DEV_MODE=1"
log_info "Cache location: ~/.fbuild/cache_dev/"
log_info "Test project: tests/uno"
echo ""

# Step 1: Clean existing cache
log_step 1 "Clean existing dev cache"
if [ -d ~/.fbuild/cache_dev ]; then
    log_info "Removing existing cache at ~/.fbuild/cache_dev/"
    rm -rf ~/.fbuild/cache_dev/
    log_success "Cache cleaned"
else
    log_info "No existing cache to clean"
fi

# Step 2: Verify cache is empty
log_step 2 "Verify cache is empty before build"
log_info "Running: uv run fbuild purge"
uv run fbuild purge > /tmp/purge_empty.txt 2>&1 || true
EXIT_CODE=$?

if grep -q "No packages cached" /tmp/purge_empty.txt; then
    log_success "Cache is empty (as expected)"
    ((TESTS_PASSED++))
else
    log_error "Expected empty cache message"
    cat /tmp/purge_empty.txt
    ((TESTS_FAILED++))
fi

assert_exit_code 1 $EXIT_CODE "Purge list mode exits with code 1"

# Step 3: Build UNO project to populate cache
log_step 3 "Build UNO project to populate cache with packages"
log_info "Running: uv run fbuild build -e uno --quick"
log_info "This may take a few minutes as it downloads toolchain and platform..."
echo ""

uv run fbuild build -e uno --quick
assert_success "Build completed successfully"

# Step 4: List cached packages
log_step 4 "List all cached packages"
log_info "Running: uv run fbuild purge"
echo ""

uv run fbuild purge > /tmp/purge_list.txt 2>&1 || true
EXIT_CODE=$?

# Display the output
cat /tmp/purge_list.txt
echo ""

# Verify output contains expected sections
if grep -q "Cached Packages" /tmp/purge_list.txt; then
    log_success "Found 'Cached Packages' header"
    ((TESTS_PASSED++))
else
    log_error "Missing 'Cached Packages' header"
    ((TESTS_FAILED++))
fi

if grep -q "Toolchains" /tmp/purge_list.txt; then
    log_success "Found 'Toolchains' section"
    ((TESTS_PASSED++))
else
    log_error "Missing 'Toolchains' section"
    ((TESTS_FAILED++))
fi

if grep -q "Platforms" /tmp/purge_list.txt; then
    log_success "Found 'Platforms' section"
    ((TESTS_PASSED++))
else
    log_error "Missing 'Platforms' section"
    ((TESTS_FAILED++))
fi

if grep -q "Total:" /tmp/purge_list.txt; then
    log_success "Found 'Total' summary"
    ((TESTS_PASSED++))
else
    log_error "Missing 'Total' summary"
    ((TESTS_FAILED++))
fi

assert_exit_code 1 $EXIT_CODE "Purge list mode exits with code 1"

# Step 5: Test dry-run mode
log_step 5 "Test dry-run mode (no actual deletion)"
log_info "Running: uv run fbuild purge all --dry-run"
echo ""

uv run fbuild purge all --dry-run > /tmp/purge_dryrun.txt 2>&1
EXIT_CODE=$?

# Display the output
cat /tmp/purge_dryrun.txt
echo ""

# Verify dry-run output
if grep -q "Dry run:" /tmp/purge_dryrun.txt; then
    log_success "Found 'Dry run' message"
    ((TESTS_PASSED++))
else
    log_error "Missing 'Dry run' message"
    ((TESTS_FAILED++))
fi

if grep -q "Would delete:" /tmp/purge_dryrun.txt; then
    log_success "Found 'Would delete' messages"
    ((TESTS_PASSED++))
else
    log_error "Missing 'Would delete' messages"
    ((TESTS_FAILED++))
fi

if grep -q "would be freed" /tmp/purge_dryrun.txt; then
    log_success "Found 'would be freed' summary"
    ((TESTS_PASSED++))
else
    log_error "Missing 'would be freed' summary"
    ((TESTS_FAILED++))
fi

assert_exit_code 0 $EXIT_CODE "Dry-run exits with code 0"

# Step 6: Verify packages still exist after dry-run
log_step 6 "Verify packages still exist after dry-run"
log_info "Running: uv run fbuild purge"
echo ""

uv run fbuild purge > /tmp/purge_after_dryrun.txt 2>&1 || true

if grep -q "Total:" /tmp/purge_after_dryrun.txt && ! grep -q "No packages cached" /tmp/purge_after_dryrun.txt; then
    log_success "Packages still exist (dry-run did not delete)"
    ((TESTS_PASSED++))
else
    log_error "Packages were deleted by dry-run (should not happen)"
    cat /tmp/purge_after_dryrun.txt
    ((TESTS_FAILED++))
fi

# Step 7: Count packages before deletion
log_step 7 "Count packages before actual deletion"
PACKAGE_COUNT=$(grep -c "^  •" /tmp/purge_after_dryrun.txt || echo "0")
log_info "Found $PACKAGE_COUNT packages in cache"

if [ "$PACKAGE_COUNT" -gt 0 ]; then
    log_success "Cache contains $PACKAGE_COUNT packages"
    ((TESTS_PASSED++))
else
    log_error "Expected packages in cache, found none"
    ((TESTS_FAILED++))
fi

# Step 8: Check for manifest files
log_step 8 "Verify manifest.json files were created"
MANIFEST_COUNT=$(find ~/.fbuild/cache_dev -name "manifest.json" | wc -l)
log_info "Found $MANIFEST_COUNT manifest.json files"

if [ "$MANIFEST_COUNT" -gt 0 ]; then
    log_success "Manifests were created during package installation"
    ((TESTS_PASSED++))

    # Show one example manifest
    FIRST_MANIFEST=$(find ~/.fbuild/cache_dev -name "manifest.json" | head -1)
    log_info "Example manifest content:"
    echo ""
    cat "$FIRST_MANIFEST" | python3 -m json.tool 2>/dev/null || cat "$FIRST_MANIFEST"
    echo ""
else
    log_error "No manifests found (packages may not have been installed with manifest support)"
    ((TESTS_FAILED++))
fi

# Step 9: Test actual purge
log_step 9 "Test actual purge (delete all packages)"
log_info "Running: uv run fbuild purge all"
echo ""

uv run fbuild purge all > /tmp/purge_all.txt 2>&1
EXIT_CODE=$?

# Display the output
cat /tmp/purge_all.txt
echo ""

# Verify purge output
if grep -q "Purging all global packages" /tmp/purge_all.txt; then
    log_success "Found 'Purging all' message"
    ((TESTS_PASSED++))
else
    log_error "Missing 'Purging all' message"
    ((TESTS_FAILED++))
fi

if grep -q "Deleted:" /tmp/purge_all.txt; then
    log_success "Found 'Deleted' messages"
    ((TESTS_PASSED++))
else
    log_error "Missing 'Deleted' messages"
    ((TESTS_FAILED++))
fi

if grep -q "Purged .* packages, freed" /tmp/purge_all.txt; then
    log_success "Found purge summary"
    ((TESTS_PASSED++))
else
    log_error "Missing purge summary"
    ((TESTS_FAILED++))
fi

assert_exit_code 0 $EXIT_CODE "Purge all exits with code 0"

# Step 10: Verify cache is empty after purge
log_step 10 "Verify cache is empty after purge"
log_info "Running: uv run fbuild purge"
echo ""

uv run fbuild purge > /tmp/purge_after_delete.txt 2>&1 || true
EXIT_CODE=$?

cat /tmp/purge_after_delete.txt
echo ""

if grep -q "No packages cached" /tmp/purge_after_delete.txt; then
    log_success "Cache is empty (all packages deleted)"
    ((TESTS_PASSED++))
else
    log_error "Cache is not empty (deletion may have failed)"
    cat /tmp/purge_after_delete.txt
    ((TESTS_FAILED++))
fi

assert_exit_code 1 $EXIT_CODE "Purge list on empty cache exits with code 1"

# Step 11: Verify no manifest files remain
log_step 11 "Verify all manifest files were deleted"
REMAINING_MANIFESTS=$(find ~/.fbuild/cache_dev -name "manifest.json" 2>/dev/null | wc -l)

if [ "$REMAINING_MANIFESTS" -eq 0 ]; then
    log_success "All manifests deleted"
    ((TESTS_PASSED++))
else
    log_error "$REMAINING_MANIFESTS manifest files still exist"
    find ~/.fbuild/cache_dev -name "manifest.json"
    ((TESTS_FAILED++))
fi

# Step 12: Test help output
log_step 12 "Test purge help output"
log_info "Running: uv run fbuild purge --help"
echo ""

uv run fbuild purge --help > /tmp/purge_help.txt 2>&1 || true

if grep -q "Manage cached packages" /tmp/purge_help.txt; then
    log_success "Help output displays correctly"
    ((TESTS_PASSED++))
else
    log_error "Help output missing or incorrect"
    cat /tmp/purge_help.txt
    ((TESTS_FAILED++))
fi

# Step 13: Rebuild to test idempotency
log_step 13 "Rebuild project to test manifest creation is repeatable"
log_info "Running: uv run fbuild build -e uno --quick --clean"
echo ""

uv run fbuild build -e uno --quick --clean
assert_success "Rebuild completed successfully"

# Verify manifests were created again
MANIFEST_COUNT_AFTER=$(find ~/.fbuild/cache_dev -name "manifest.json" | wc -l)
log_info "Found $MANIFEST_COUNT_AFTER manifest.json files after rebuild"

if [ "$MANIFEST_COUNT_AFTER" -gt 0 ]; then
    log_success "Manifests created on rebuild (idempotent)"
    ((TESTS_PASSED++))
else
    log_error "No manifests created on rebuild"
    ((TESTS_FAILED++))
fi

# Final Summary
echo ""
echo -e "${BLUE}===================================================================${NC}"
echo -e "${BLUE}                         Test Summary                              ${NC}"
echo -e "${BLUE}===================================================================${NC}"
echo ""

if [ $TESTS_FAILED -eq 0 ]; then
    echo -e "${GREEN}╔═══════════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${GREEN}║                     ALL TESTS PASSED! ✓                           ║${NC}"
    echo -e "${GREEN}╚═══════════════════════════════════════════════════════════════════╝${NC}"
else
    echo -e "${RED}╔═══════════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${RED}║                     SOME TESTS FAILED! ✗                          ║${NC}"
    echo -e "${RED}╚═══════════════════════════════════════════════════════════════════╝${NC}"
fi

echo ""
echo -e "  ${GREEN}Passed:${NC} $TESTS_PASSED"
echo -e "  ${RED}Failed:${NC} $TESTS_FAILED"
echo -e "  ${BLUE}Total:${NC}  $((TESTS_PASSED + TESTS_FAILED))"
echo ""

# Cleanup temporary files
rm -f /tmp/purge_*.txt

# Exit with appropriate code
if [ $TESTS_FAILED -eq 0 ]; then
    exit 0
else
    exit 1
fi
