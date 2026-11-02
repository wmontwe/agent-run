# lib/lock.sh

test_lock_and_unlock() {
    memory_lock "$T/lock" 1
    assert_eq "$$" "$(cat "$T/lock/pid")"
    memory_unlock "$T/lock"
    assert_missing "$T/lock"
}

test_lock_held_by_live_process_times_out() {
    mkdir lock && echo "$PPID" >lock/pid
    assert_dies "memory is locked by process $PPID" memory_lock "$T/lock" 1
    assert_exists "$T/lock"
}

test_stale_lock_taken_over() {
    sleep 0 &
    local dead=$!
    wait "$dead"
    mkdir lock && echo "$dead" >lock/pid
    memory_lock "$T/lock" 1
    assert_eq "$$" "$(cat "$T/lock/pid")"
}

test_waits_for_release() {
    mkdir lock
    ( sleep 1; rm -rf "$T/lock" ) &
    memory_lock "$T/lock" 5
    assert_eq "$$" "$(cat "$T/lock/pid")"
    wait
}

test_unlock_leaves_foreign_lock() {
    mkdir lock && echo "$PPID" >lock/pid
    memory_unlock "$T/lock"
    assert_exists "$T/lock"
}
