<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Command;

use OCA\FreeSize\Service\Agent;
use Symfony\Component\Console\Command\Command;
use Symfony\Component\Console\Input\InputInterface;
use Symfony\Component\Console\Output\OutputInterface;

class Status extends Command {
	public function __construct(
		private Agent $agent,
	) {
		parent::__construct();
	}

	protected function configure(): void {
		$this->setName('freesize:status')
			->setDescription('Show the Hub, the approver and the users FreeSize covers');
	}

	protected function execute(InputInterface $input, OutputInterface $output): int {
		$state = $this->agent->state();
		if (!$state->paired()) {
			$output->writeln('Not paired with a Hub: occ freesize:pair LINK');
			return 0;
		}
		$status = $state->status();
		$approver = $state->approver();
		$when = fn (int $t) => $t === 0 ? 'never' : date('Y-m-d H:i:s', $t);
		$output->writeln("Hub: {$state->hubName()} at {$state->hub()}");
		$output->writeln("This Nextcloud: \"{$state->name()}\", fingerprint {$state->fingerprint()}");
		$output->writeln('Approver: ' . ($approver === null ? 'none, so requests are refused' : "{$approver['name']} ({$state->approverFingerprint()})"));
		$output->writeln('Users covered: ' . ($state->users() === [] ? 'none' : implode(', ', $state->users())));
		$output->writeln('Trash bin: ' . ($this->agent->trashBin() ? 'on' : 'off, so nothing is deleted'));
		$output->writeln('Grants: ' . count($state->grants(time())));
		$output->writeln("Last report: {$when($status['last_report'])}, last poll: {$when($status['last_poll'])}");
		if ($status['last_error'] !== '') {
			$output->writeln("Last error: {$status['last_error']}");
		}
		return 0;
	}
}
