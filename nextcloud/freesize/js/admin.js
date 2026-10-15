// SPDX-License-Identifier: AGPL-3.0-or-later
// The FreeSize admin page: plain JavaScript on Nextcloud's own globals, so
// the app needs no build step.
(function () {
	'use strict'

	const root = () => document.getElementById('freesize-body')
	const message = () => document.getElementById('freesize-message')
	let state = null
	let hubFingerprint = null

	function el(tag, attrs, ...children) {
		const node = document.createElement(tag)
		for (const [key, value] of Object.entries(attrs || {})) {
			if (key === 'onclick' || key === 'onsubmit' || key === 'oninput') {
				node[key] = value
			} else if (value !== false && value !== null && value !== undefined) {
				node.setAttribute(key, value === true ? '' : value)
			}
		}
		for (const child of children.flat()) {
			if (child !== null && child !== undefined && child !== false) {
				node.append(typeof child === 'string' ? document.createTextNode(child) : child)
			}
		}
		return node
	}

	function url(path) {
		return OC.generateUrl('/apps/freesize' + path)
	}

	function say(text, error) {
		const box = message()
		box.hidden = !text
		box.textContent = text || ''
		box.className = 'freesize-message' + (error ? ' freesize-error' : '')
	}

	function when(seconds) {
		return seconds ? new Date(seconds * 1000).toLocaleString() : 'never'
	}

	async function confirmed() {
		const confirmation = OC.PasswordConfirmation
		if (confirmation && confirmation.requiresPasswordConfirmation()) {
			await new Promise((resolve, reject) => confirmation.requirePasswordConfirmation(resolve, {}, reject))
		}
	}

	async function call(method, path, body, needsPassword) {
		try {
			if (needsPassword) {
				await confirmed()
			}
			const response = await fetch(url(path), {
				method,
				headers: {
					requesttoken: OC.requestToken,
					'Content-Type': 'application/json',
					Accept: 'application/json',
				},
				body: body ? JSON.stringify(body) : undefined,
			})
			const answer = await response.json()
			if (answer.state) {
				state = answer.state
			}
			if (answer.error) {
				hubFingerprint = answer.hub_fingerprint || null
				say(answer.error, true)
			} else {
				hubFingerprint = null
				say(answer.said || '', false)
			}
			render()
			return answer
		} catch (e) {
			say(e && e.message ? e.message : 'That didn\'t go through. Try again.', true)
			return null
		}
	}

	function busy(button, work) {
		return async (event) => {
			if (event) {
				event.preventDefault()
			}
			button.disabled = true
			try {
				await work()
			} finally {
				button.disabled = false
			}
		}
	}

	function card(title, ...children) {
		return el('div', { class: 'freesize-card' }, el('h3', {}, title), ...children)
	}

	function hubCard() {
		if (!state.paired) {
			const link = el('textarea', { rows: 3, placeholder: 'freesize://pair?hub=…&code=…&key=…' })
			const code = el('input', { type: 'text', placeholder: 'Only with an address: 7KQ2-M9XD-4HRT' })
			const fingerprint = el('input', { type: 'text', placeholder: 'ABCD-EFGH-JKMN-PQRS', value: hubFingerprint || '' })
			const pair = el('button', { class: 'button primary', type: 'submit' }, 'Pair')
			const form = el('form', {},
				el('label', {}, 'The link from the Hub\'s "Pair a device", or the Hub\'s address', link),
				el('label', {}, 'Pairing code', code),
				hubFingerprint
					? el('label', {}, 'The Hub\'s fingerprint, as the dashboard shows it under Settings. Check it matches ' + hubFingerprint + ' before you pair.', fingerprint)
					: null,
				pair)
			form.onsubmit = busy(pair, () => call('POST', '/admin/pair', {
				link: link.value, code: code.value, fingerprint: fingerprint.value,
			}, true))
			return card('Pair with your Hub',
				el('p', {}, 'On the Hub\'s dashboard, choose Pair a device and copy its link.'),
				form)
		}
		const name = el('input', { type: 'text', value: state.name })
		const save = el('button', { class: 'button' }, 'Save')
		save.onclick = busy(save, () => call('POST', '/admin/name', { name: name.value }))
		const run = el('button', { class: 'button primary' }, 'Report now')
		run.onclick = busy(run, () => call('POST', '/admin/run'))
		const forget = el('button', { class: 'button freesize-danger' }, 'Forget the Hub')
		forget.onclick = busy(forget, async () => {
			if (window.confirm('Forget ' + state.hub_name + '? The Hub stops getting reports and requests from this Nextcloud until it\'s paired again.')) {
				await call('POST', '/admin/unpair', null, true)
			}
		})
		const status = state.status || {}
		return card('Hub',
			el('p', {}, 'Paired with ', el('strong', {}, state.hub_name), ' at ' + state.hub + '.'),
			el('p', {}, 'This Nextcloud\'s fingerprint: ', el('code', {}, state.fingerprint)),
			el('label', { class: 'freesize-inline' }, 'Shown on the Hub as', name, save),
			el('p', { class: 'freesize-hint' }, 'Last report: ' + when(status.last_report) + '. Last asked the Hub for requests: ' + when(status.last_poll) + '.'),
			status.last_error ? el('p', { class: 'freesize-error-text' }, 'Last problem: ' + status.last_error) : null,
			el('div', { class: 'freesize-buttons' }, run, forget))
	}

	function approverCard() {
		const fingerprint = el('input', { type: 'text', placeholder: 'ABCD-EFGH-JKMN-PQRS' })
		const set = el('button', { class: 'button primary', type: 'submit' }, state.approver ? 'Change approver' : 'Set approver')
		const form = el('form', { class: 'freesize-inline' }, fingerprint, set)
		form.onsubmit = busy(set, () => call('POST', '/admin/approver', { fingerprint: fingerprint.value }, true))
		const parts = [
			el('p', {}, 'Nobody sits at a server, so every delete the Hub asks for waits until the approver allows it on their phone. Without an approver, requests are refused.'),
		]
		if (state.approver) {
			const clear = el('button', { class: 'button' }, 'Remove approver')
			clear.onclick = busy(clear, () => call('POST', '/admin/approver', { fingerprint: '' }, true))
			parts.push(el('p', {}, 'Deletes are approved by ', el('strong', {}, state.approver.name), ' (', el('code', {}, state.approver.fingerprint), ').'))
			parts.push(el('div', { class: 'freesize-buttons' }, clear))
			if (state.grants > 0) {
				const end = el('button', { class: 'button' }, 'End grants')
				end.onclick = busy(end, () => call('POST', '/admin/grants/end'))
				parts.push(el('p', {}, state.grants + ' grant(s) running: the approver allowed deletes for a while without asking each time. ', end))
			}
		}
		parts.push(el('label', {}, 'The approver\'s fingerprint, from Settings, Hub on their phone', form))
		return card('Approver', ...parts)
	}

	function usersCard() {
		const list = el('ul', { class: 'freesize-users' })
		for (const user of state.users) {
			const remove = el('button', { class: 'button' }, 'Remove')
			remove.onclick = busy(remove, () => call('POST', '/admin/users', { remove: user.id }))
			list.append(el('li', {},
				el('span', {}, el('strong', {}, user.name), ' ' + user.id + (user.email ? ' · ' + user.email : ' · no email, so only Hub admins see it')),
				remove))
		}
		const search = el('input', { type: 'search', placeholder: 'Find a user to add' })
		const found = el('ul', { class: 'freesize-users' })
		let timer = null
		search.oninput = () => {
			clearTimeout(timer)
			timer = setTimeout(async () => {
				found.replaceChildren()
				if (!search.value.trim()) {
					return
				}
				const response = await fetch(url('/admin/users?search=' + encodeURIComponent(search.value)), {
					headers: { requesttoken: OC.requestToken, Accept: 'application/json' },
				})
				const answer = await response.json()
				for (const user of answer.users || []) {
					if (state.users.some((u) => u.id === user.id)) {
						continue
					}
					const add = el('button', { class: 'button primary' }, 'Add')
					add.onclick = busy(add, () => call('POST', '/admin/users', { add: user.id }))
					found.append(el('li', {}, el('span', {}, el('strong', {}, user.name), ' ' + user.id), add))
				}
			}, 250)
		}
		return card('Users it covers',
			el('p', {}, 'The Hub sees the files of these users only. Each person signed in to the Hub sees only the account with their email address; Hub admins see them all.'),
			state.users.length ? list : el('p', { class: 'freesize-hint' }, 'No users yet.'),
			search, found)
	}

	function warnings() {
		const out = []
		if (!state.trash_bin) {
			out.push(el('p', { class: 'freesize-warning' }, 'The Deleted files app (trash bin) is off, so FreeSize deletes nothing: a delete couldn\'t be undone. Turn it on under Apps.'))
		}
		if (state.cron !== 'cron' && state.cron !== 'webcron') {
			out.push(el('p', { class: 'freesize-warning' }, 'Background jobs run only when someone opens Nextcloud (AJAX). Choose Cron under Basic settings, so FreeSize reports and carries out approved deletes on time.'))
		}
		return out
	}

	function render() {
		const body = root()
		if (!body || !state) {
			return
		}
		body.replaceChildren(...warnings(), hubCard(), ...(state.paired ? [approverCard(), usersCard()] : []))
	}

	document.addEventListener('DOMContentLoaded', () => {
		try {
			state = OCP.InitialState.loadState('freesize', 'state')
		} catch (e) {
			state = null
		}
		if (state) {
			render()
		} else {
			call('GET', '/admin/state')
		}
	})
})()
