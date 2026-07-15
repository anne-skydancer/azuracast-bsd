<template>
    <form
        class="form vue-form"
        @submit.prevent="submit"
    >
        <section
            class="card"
            role="region"
            aria-labelledby="hdr_edit_engine_config"
        >
            <div class="card-header text-bg-primary">
                <h2
                    id="hdr_edit_engine_config"
                    class="card-title"
                >
                    {{ $gettext('Edit Engine Configuration') }}
                </h2>
            </div>

            <info-card>
                <p class="card-text">
                    {{
                        $gettext('This is the raw configuration used by the AzuraCast Engine to run your station\'s AutoDJ and live broadcasting. You can edit it directly here as an alternative to using the individual settings pages.')
                    }}
                </p>
                <p class="card-text">
                    {{
                        $gettext('Only recognized settings (replaygain, crossfade and live broadcast harbor options) are saved back to your station; other values shown here (such as ports, API keys, callback URLs and file paths) are managed automatically and any changes to them will be ignored.')
                    }}
                </p>
            </info-card>

            <loading :loading="isLoading" lazy>
                <div class="card-body">
                    <button
                        type="submit"
                        class="btn btn-primary mb-2"
                    >
                        {{ $gettext('Save Changes') }}
                    </button>

                    <form-group
                        id="form_edit_engine_config"
                        class="mb-0"
                    >
                        <template #default="{id}">
                            <codemirror-textarea
                                :id="id"
                                v-model="configText"
                                mode="toml"
                            />
                        </template>
                    </form-group>

                    <button
                        type="submit"
                        class="btn btn-primary mt-2"
                    >
                        {{ $gettext('Save Changes') }}
                    </button>
                </div>
            </loading>
        </section>
    </form>
</template>

<script setup lang="ts">
import { onMounted, ref } from "vue";
import CodemirrorTextarea from "~/components/Common/CodemirrorTextarea.vue";
import InfoCard from "~/components/Common/InfoCard.vue";
import Loading from "~/components/Common/Loading.vue";
import { useNotify } from "~/components/Common/Toasts/useNotify.ts";
import FormGroup from "~/components/Form/FormGroup.vue";
import { useApiRouter } from "~/functions/useApiRouter.ts";
import { useMayNeedRestart } from "~/functions/useMayNeedRestart";
import { useAxios } from "~/vendor/axios";

const { getStationApiUrl } = useApiRouter();
const settingsUrl = getStationApiUrl("/engine-config");

const configText = ref<string>("");

const isLoading = ref(true);

const { mayNeedRestart } = useMayNeedRestart();

const { axios } = useAxios();

const relist = async () => {
    isLoading.value = true;

    try {
        const { data } = await axios.get(settingsUrl.value);
        configText.value = data.config ?? "";
    } finally {
        isLoading.value = false;
    }
};

onMounted(relist);

const { notifySuccess } = useNotify();

const submit = async () => {
    await axios({
        method: "PUT",
        url: settingsUrl.value,
        data: {
            config: configText.value,
        },
    });

    notifySuccess();
    mayNeedRestart();
    await relist();
};
</script>
