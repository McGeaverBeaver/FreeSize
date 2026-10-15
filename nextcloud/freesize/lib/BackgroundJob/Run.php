<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\BackgroundJob;

use OCA\FreeSize\Service\Agent;
use OCP\AppFramework\Utility\ITimeFactory;
use OCP\BackgroundJob\TimedJob;

/** Every five minutes, with Nextcloud's cron: report and take requests. */
class Run extends TimedJob {
	public function __construct(
		ITimeFactory $time,
		private Agent $agent,
	) {
		parent::__construct($time);
		$this->setInterval(5 * 60);
		$this->setTimeSensitivity(self::TIME_SENSITIVE);
	}

	protected function run($argument): void {
		$this->agent->run();
	}
}
