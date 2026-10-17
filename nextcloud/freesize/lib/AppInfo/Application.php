<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\AppInfo;

use OCP\AppFramework\App;
use OCP\AppFramework\Bootstrap\IBootContext;
use OCP\AppFramework\Bootstrap\IBootstrap;
use OCP\AppFramework\Bootstrap\IRegistrationContext;

class Application extends App implements IBootstrap {
	public const APP_ID = 'freesize';
	/** Sent to the Hub as the app's version; keep in step with appinfo/info.xml. */
	public const VERSION = '0.20.3';

	public function __construct(array $urlParams = []) {
		parent::__construct(self::APP_ID, $urlParams);
	}

	public function register(IRegistrationContext $context): void {
	}

	public function boot(IBootContext $context): void {
	}
}
