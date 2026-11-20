// The token comes out of the address bar before the app starts, and so
// before anything connects.
import { takeToken } from './link.ts'
import { mount } from 'svelte'
import App from './App.svelte'
import './chat/theme.css'

takeToken()
mount(App, { target: document.getElementById('app')! })
