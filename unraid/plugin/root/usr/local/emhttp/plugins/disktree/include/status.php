<?php
// DiskTree for Unraid: what the background part found, for the page.
require_once __DIR__.'/common.php';

$text = @file_get_contents("$dt_data/status.json");
$status = $text ? json_decode($text, true) : null;
dt_reply([
  'status' => $status,
  // It writes at least once a minute while it runs.
  'running' => $status && time() - (int)($status['updated'] ?? 0) < 180,
  'config' => dt_config(),
  'now' => time(),
]);
